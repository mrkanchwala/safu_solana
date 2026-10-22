//! Stateful program fuzz: random sequences of every liquidity path (stake, partial and whole
//! withdraw, emergency exit, backing and its withdrawal, claims end to end, yield, treasury,
//! pause, settings through the timelock, donations to the vault, covered-wallet registration)
//! against the real program in LiteSVM, all in USDC. Ordinary refusals are expected and ignored;
//! the books are checked after every step, success or not.
//!
//! Ignored in normal runs. On the VPS:
//!   FUZZ_SECS=900 cargo test --release --test fuzz_stateful -- --ignored --nocapture
//! Replay one failure: FUZZ_SEED=<seed> FUZZ_EPISODES=1 (same command).
//!
//! Known by design, measured not failed: a paid claim beyond the forfeited stake is not written
//! off stakers' or backers' books (same as the multichain pool), so after such a payout the books
//! can exceed what the pool holds. The check allows exactly the claim payouts made (`paid`) and
//! reports the largest gap seen.

mod common;

use anchor_lang::prelude::Pubkey;
use anchor_lang::InstructionData;
use common::*;
use pool_core::params::{DEMO_BUILD, SECONDS_PER_DAY, SECONDS_PER_HOUR};
use pool_core::settings::SETTING_COUNT;
use safu_pool::approval::ClaimApproval;
use safu_pool::constants::*;
use safu_pool::state::{
    BackerRecord, Claim, ClaimStatus, CoveredWallet, Pool, StakeRecord, StakerWallets,
};
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const STEPS: usize = 250;
const STAKERS: usize = 4;
const BACKERS: usize = 3;
/// Covered-wallet hashes drawn from a small set, so repeats, taken wallets and the limit all happen.
const WALLET_HASHES: u8 = 6;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
    fn range(&mut self, lo: u64, hi: u64) -> u64 {
        lo + self.below(hi.saturating_sub(lo).saturating_add(1))
    }
    fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

struct Staker {
    kp: Keypair,
    beneficiary: Pubkey,
}

#[derive(Clone, Copy)]
struct ClaimRef {
    staker: usize,
    tx: [u8; 32],
}

struct World {
    env: Env,
    rng: Rng,
    stakers: Vec<Staker>,
    backers: Vec<Keypair>,
    claims: Vec<ClaimRef>,
    next_tx: u64,
    /// Claim money paid out (entitlement streamed), the one thing allowed to leave the books short.
    paid: u64,
    max_gap: u64,
    trace: Vec<String>,
}

/// What a step did, for the step-specific checks.
enum Did {
    Other,
    /// Principal left through the free-capital rule.
    CapitalOut {
        staker: Option<usize>,
        amount: u64,
        beneficiary: Pubkey,
        before: u64,
    },
    Pause,
    Expire,
    WithdrawYield,
    Emergency,
}

impl World {
    fn new(seed: u64) -> World {
        let mut env = Env::new();
        let mut stakers = Vec::new();
        for _ in 0..STAKERS {
            let kp = env.funded(200 * unit());
            let beneficiary = Keypair::new().pubkey();
            env.open_usdc(&beneficiary);
            stakers.push(Staker { kp, beneficiary });
        }
        let backers = (0..BACKERS).map(|_| env.funded(500 * unit())).collect();
        World {
            env,
            rng: Rng(seed.max(1)),
            stakers,
            backers,
            claims: Vec::new(),
            next_tx: 1,
            paid: 0,
            max_gap: 0,
            trace: Vec::new(),
        }
    }

    fn pool(&self) -> Pool {
        self.env.pool_state()
    }

    fn stake_rec(&self, i: usize) -> Option<StakeRecord> {
        let a = self.env.stake_record(&self.stakers[i].kp.pubkey());
        self.env.exists(&a).then(|| self.env.read(&a))
    }

    fn backer_rec(&self, i: usize) -> Option<BackerRecord> {
        self.backer_rec_of(&self.backers[i].pubkey())
    }

    fn backer_rec_of(&self, owner: &Pubkey) -> Option<BackerRecord> {
        let a = self.env.backer_record(owner);
        self.env.exists(&a).then(|| self.env.read(&a))
    }

    /// Every backer record: the backers', and the stakers' (a stake above the max backs the rest).
    fn all_backer_recs(&self) -> Vec<BackerRecord> {
        let owners = self.backers.iter().map(|k| k.pubkey());
        let stakers = self.stakers.iter().map(|s| s.kp.pubkey());
        owners
            .chain(stakers)
            .filter_map(|o| self.backer_rec_of(&o))
            .collect()
    }

    /// The claim first in line, if anyone waits.
    fn queue_head_claim(&self) -> Option<ClaimRef> {
        let p = self.pool();
        if p.queue_head >= p.queue_tail {
            return None;
        }
        let entry = self.env.queue_entry(p.queue_head);
        if !self.env.exists(&entry) {
            return None;
        }
        let claim = self.env.queue_state(p.queue_head).claim;
        self.claims.iter().copied().find(|c| {
            self.env
                .claim_addr(&self.stakers[c.staker].kp.pubkey(), &c.tx)
                == claim
        })
    }

    fn claim(&self, c: &ClaimRef) -> Option<Claim> {
        let a = self
            .env
            .claim_addr(&self.stakers[c.staker].kp.pubkey(), &c.tx);
        self.env.exists(&a).then(|| self.env.read(&a))
    }

    fn min_max(&self) -> (u64, u64) {
        pool_core::stake::bounds(&self.pool().settings()).unwrap_or((1, 1))
    }

    /// Edge values around the rules, plus random ones.
    fn amount(&mut self, stake: u64) -> u64 {
        let (min, max) = self.min_max();
        let r = self.rng.range(1, max.max(2));
        let pick = [
            0,
            1,
            min,
            min.saturating_sub(1),
            max,
            max + 1,
            stake,
            stake + 1,
            stake.saturating_sub(min),
            stake.saturating_sub(min) + 1,
            stake / 2,
            r,
            r,
            r,
        ];
        pick[self.rng.below(pick.len() as u64) as usize]
    }

    fn send(
        &mut self,
        ixs: &[anchor_lang::solana_program::instruction::Instruction],
        who: &Keypair,
    ) -> bool {
        self.env.send_logs(ixs, &[who]).is_ok()
    }

    /// A claim not yet finished (mostly), so claim steps reach the whole path.
    fn open_claim(&mut self) -> Option<ClaimRef> {
        let live: Vec<ClaimRef> = self
            .claims
            .iter()
            .copied()
            .filter(|c| {
                self.claim(c).is_some_and(|cl| {
                    !matches!(
                        cl.status,
                        ClaimStatus::Completed | ClaimStatus::Cancelled | ClaimStatus::Expired
                    )
                })
            })
            .collect();
        let from = if live.is_empty() || self.rng.chance(10) {
            &self.claims
        } else {
            &live
        };
        if from.is_empty() {
            return None;
        }
        Some(from[self.rng.below(from.len() as u64) as usize])
    }

    fn transition(&mut self, c: ClaimRef, data: impl InstructionData) -> bool {
        let s = self.stakers[c.staker].kp.pubkey();
        let ix = self.env.transition_ix(&s, &c.tx, data);
        let payer = self.env.admin.insecure_clone();
        self.send(&[ix], &payer)
    }

    /// One random step. Returns its name, whether it went through, and what it did.
    fn step(&mut self) -> (&'static str, bool, Did) {
        let admin = self.env.admin.insecure_clone();
        let co = self.env.co_signer.insecure_clone();
        let si = self.rng.below(STAKERS as u64) as usize;
        let bi = self.rng.below(BACKERS as u64) as usize;
        let sk = self.stakers[si].kp.insecure_clone();
        let spk = sk.pubkey();
        let ben = self.stakers[si].beneficiary;
        let bk = self.backers[bi].insecure_clone();
        let bpk = bk.pubkey();
        match self.rng.below(40) {
            0 | 1 => {
                let a = self.amount(0);
                let ix = self.env.stake_ix(&spk, a, ben);
                ("stake", self.send(&[ix], &sk), Did::Other)
            }
            2 | 3 => {
                let stake = self.stake_rec(si).map(|r| r.amount).unwrap_or(0);
                let a = self.amount(stake);
                let before = self.env.usdc(&ben);
                let ix = self.env.withdraw_part_ix(&spk, &ben, a);
                let ok = self.send(&[ix], &sk);
                let did = Did::CapitalOut {
                    staker: Some(si),
                    amount: a,
                    beneficiary: ben,
                    before,
                };
                ("withdraw", ok, did)
            }
            4 => {
                let stake = self.stake_rec(si).map(|r| r.amount).unwrap_or(0);
                let a = self.amount(stake);
                let ix = self.env.emergency_exit_part_ix(&spk, a);
                ("emergency_exit", self.send(&[ix], &sk), Did::Emergency)
            }
            5 => {
                let ix = self.env.claim_yield_ix(&spk, &ben);
                ("claim_yield", self.send(&[ix], &sk), Did::Other)
            }
            6 => {
                let a = self.rng.range(1, 60 * unit());
                let ix = self.env.back_ix(&bpk, a);
                ("back", self.send(&[ix], &bk), Did::Other)
            }
            7 => {
                let ix = self.env.mature_ix(&bpk);
                ("mature_backing", self.send(&[ix], &bk), Did::Other)
            }
            8 => {
                let have = self.backer_rec(bi).map(|r| r.amount).unwrap_or(0);
                let a = [1, have / 3, have, have + 1, self.rng.range(1, have.max(1))]
                    [self.rng.below(5) as usize];
                let ix = self.env.request_backer_ix(&bpk, a);
                (
                    "request_backer_withdrawal",
                    self.send(&[ix], &bk),
                    Did::Other,
                )
            }
            9 => {
                let ix = self
                    .env
                    .backer_only_ix(&bpk, safu_pool::instruction::CancelBackerWithdrawal {});
                (
                    "cancel_backer_withdrawal",
                    self.send(&[ix], &bk),
                    Did::Other,
                )
            }
            10 | 11 => {
                let pre = self.backer_rec(bi);
                let reserved = self.pool().backer_yield_reserved;
                let before = self.env.usdc(&bpk);
                let ix = self.env.complete_backer_ix(&bpk);
                let ok = self.send(&[ix], &bk);
                // One drip part of principal (the rest of the request once it closes), plus the
                // yield paid with it.
                let principal = match (pre, self.backer_rec(bi)) {
                    (Some(b), Some(a)) if a.withdraw_amount == 0 => {
                        b.withdraw_amount - b.withdraw_paid
                    }
                    (Some(b), Some(a)) => a.withdraw_paid - b.withdraw_paid,
                    _ => 0,
                };
                let yield_paid = reserved.saturating_sub(self.pool().backer_yield_reserved);
                let amount = principal + yield_paid;
                let did = Did::CapitalOut {
                    staker: None,
                    amount,
                    beneficiary: bpk,
                    before,
                };
                ("complete_backer_withdrawal", ok, did)
            }
            12 => {
                let ix = self.env.claim_backer_yield_ix(&bpk);
                ("claim_backer_yield", self.send(&[ix], &bk), Did::Other)
            }
            13 | 14 => {
                let Some(rec) = self.stake_rec(si) else {
                    return ("submit_claim", false, Did::Other);
                };
                let tier = [TIER_A, TIER_B, TIER_C][self.rng.below(3) as usize];
                // This stake's own ceiling: its ratios, or the live ones where higher.
                let ratios = rec.tier_ratios(self.pool().settings().tier_ratios());
                let top = pool_core::claim::tier_cap(rec.amount, tier, ratios)
                    .unwrap()
                    .max(1);
                let entitlement =
                    [top, top + 1, self.rng.range(1, top), 1][self.rng.below(4) as usize];
                let mut tx = [0u8; 32];
                tx[..8].copy_from_slice(&self.next_tx.to_le_bytes());
                self.next_tx += 1;
                let now = self.env.now;
                let hack = [
                    now,
                    rec.staked_at,
                    self.rng.range(rec.staked_at.max(0) as u64, now as u64) as i64,
                ][self.rng.below(3) as usize];
                let a = ClaimApproval {
                    staker: spk,
                    tx_hash: tx,
                    entitlement,
                    tier,
                    hack_timestamp: hack,
                    deadline: now + MAX_APPROVAL_WINDOW_SECS,
                };
                let ok = self.env.submit(&a).is_ok();
                if ok {
                    self.claims.push(ClaimRef { staker: si, tx });
                }
                ("submit_claim", ok, Did::Other)
            }
            15 | 31 | 32 => match self.open_claim() {
                Some(c) => (
                    "unlock_pending_claim",
                    self.transition(c, safu_pool::instruction::UnlockPendingClaim {}),
                    Did::Other,
                ),
                None => ("unlock_pending_claim", false, Did::Other),
            },
            16 => match self.queue_head_claim() {
                Some(c) => {
                    let s = self.stakers[c.staker].kp.pubkey();
                    let payer = self.env.admin.insecure_clone();
                    if self.rng.chance(2) {
                        let ix = self.env.release_ix(&s, &c.tx);
                        (
                            "try_release_queued_claim",
                            self.send(&[ix], &payer),
                            Did::Other,
                        )
                    } else {
                        let ix = self.env.advance_ix(&s, &c.tx);
                        ("advance_queue", self.send(&[ix], &payer), Did::Other)
                    }
                }
                None => ("try_release_queued_claim", false, Did::Other),
            },
            17 | 18 | 33 | 34 => match self.open_claim() {
                Some(c) => {
                    let s = self.stakers[c.staker].kp.insecure_clone();
                    let ix = self.env.approve_claim_ix(&s.pubkey(), &c.tx);
                    ("approve_claim", self.send(&[ix], &s), Did::Other)
                }
                None => ("approve_claim", false, Did::Other),
            },
            19 | 20 | 35 | 36 | 37 => match self.open_claim() {
                Some(c) => {
                    let s = self.stakers[c.staker].kp.insecure_clone();
                    let b = self.stakers[c.staker].beneficiary;
                    let before = self.claim(&c);
                    let ix = self.env.stream_ix(&s.pubkey(), &c.tx, &b);
                    let ok = self.send(&[ix], &s);
                    if ok {
                        let after = self.claim(&c);
                        let paid = match (before, after) {
                            (Some(b), Some(a)) => a.streamed - b.streamed,
                            (Some(b), None) => b.entitlement - b.streamed,
                            _ => 0,
                        };
                        self.paid += paid;
                    }
                    ("claim_stream", ok, Did::Other)
                }
                None => ("claim_stream", false, Did::Other),
            },
            21 => match self.open_claim() {
                Some(c) => {
                    let s = self.stakers[c.staker].kp.pubkey();
                    let ix = self.env.cancel_claim_ix(&admin.pubkey(), &s, &c.tx);
                    ("cancel_claim", self.send(&[ix], &admin), Did::Other)
                }
                None => ("cancel_claim", false, Did::Other),
            },
            22 => match self.open_claim() {
                Some(c) => {
                    let (name, ok) = match self.rng.below(3) {
                        0 => (
                            "expire_pending_approval",
                            self.transition(c, safu_pool::instruction::ExpirePendingApproval {}),
                        ),
                        1 => (
                            "expire_stale_claim",
                            self.transition(c, safu_pool::instruction::ExpireStaleClaim {}),
                        ),
                        _ => (
                            "expire_queued_claim",
                            self.transition(c, safu_pool::instruction::ExpireQueuedClaim {}),
                        ),
                    };
                    (name, ok, Did::Expire)
                }
                None => ("expire", false, Did::Other),
            },
            24 => {
                let p = self.pool();
                let a = [1, p.protocol_yield_balance, p.protocol_yield_balance + 1]
                    [self.rng.below(3) as usize];
                let t = self.env.treasury.pubkey();
                let ix = self.env.withdraw_yield_ix(&admin.pubkey(), &t, a);
                (
                    "withdraw_yield",
                    self.send(&[ix], &admin),
                    Did::WithdrawYield,
                )
            }
            25 => {
                let ix = self.env.admin_ix(safu_pool::instruction::Pause {});
                ("pause", self.send(&[ix], &admin), Did::Pause)
            }
            26 => {
                let ix = self.env.admin_ix(safu_pool::instruction::Unpause {});
                ("unpause", self.send(&[ix], &admin), Did::Other)
            }
            38 => {
                let a = self.rng.range(1, 2 * MAX_ROUNDING_DRAW);
                let ix = self.env.fund_reserve_ix(&bpk, a);
                ("fund_rounding_reserve", self.send(&[ix], &bk), Did::Other)
            }
            27 => {
                // Propose + co-sign a random in-range value; executes after the timelock.
                let key = self.rng.below(SETTING_COUNT as u64) as u8;
                let k = SettingKey::from_index(key).unwrap();
                let (lo, hi) = k.bounds();
                let cur = self.env.setting(k);
                let hi = if hi == i64::MAX {
                    cur.saturating_mul(3)
                } else {
                    hi
                };
                let v = [lo, hi, cur, self.rng.range(lo as u64, hi as u64) as i64]
                    [self.rng.below(4) as usize];
                let p = self.env.propose_ix(key, v);
                let ok = self.send(&[p], &admin) && {
                    let a = self.env.approve_setting_ix(&co.pubkey(), key, v);
                    self.send(&[a], &co)
                };
                ("propose_approve_setting", ok, Did::Other)
            }
            28 => {
                let key = self.rng.below(SETTING_COUNT as u64) as u8;
                let ix = self.env.execute_setting_ix(key);
                ("execute_setting", self.send(&[ix], &admin), Did::Other)
            }
            23 | 29 => {
                // Someone sends USDC straight to the vault: never stake, only surplus.
                let v = self.env.vault();
                let a = self.rng.range(1, 50 * unit());
                self.env.add_token(&v, a);
                ("donation", true, Did::Other)
            }
            39 => {
                // The backend's registry write: any staker, a hash from the small set.
                let writer = self.env.writer.insecure_clone();
                let h = [self.rng.below(WALLET_HASHES as u64) as u8 + 1; 32];
                let ix = self.env.register_ix(&writer.pubkey(), spk, h);
                ("register_wallet", self.send(&[ix], &writer), Did::Other)
            }
            _ => {
                let secs = [
                    60,
                    SECONDS_PER_HOUR,
                    SECONDS_PER_DAY,
                    7 * SECONDS_PER_DAY,
                    31 * SECONDS_PER_DAY,
                    self.rng.range(1, 40 * SECONDS_PER_DAY as u64) as i64,
                ][self.rng.below(6) as usize];
                self.env.warp(secs);
                ("warp", true, Did::Other)
            }
        }
    }

    /// Covered wallets: at most the limit per staker, spare bytes untouched, and every wallet record
    /// and staker list agree both ways (a wallet belongs to exactly the staker that lists it).
    fn check_covered_wallets(&self) -> Result<(), String> {
        for s in &self.stakers {
            let pk = s.kp.pubkey();
            let a = self.env.staker_wallets(&pk);
            if !self.env.exists(&a) {
                continue;
            }
            let w: StakerWallets = self.env.read(&a);
            if w.count > MAX_COVERED_WALLETS || w.reserved.iter().any(|b| *b != 0) {
                return Err(format!("staker wallets out of bounds: count {}", w.count));
            }
            for h in w.hashes() {
                let c: CoveredWallet = self.env.read(&self.env.covered(h));
                if c.staker != pk || c.wallet_hash != *h {
                    return Err(format!("listed wallet {:?} belongs to {}", h[0], c.staker));
                }
            }
        }
        for n in 1..=WALLET_HASHES {
            let a = self.env.covered(&[n; 32]);
            if !self.env.exists(&a) {
                continue;
            }
            let c: CoveredWallet = self.env.read(&a);
            let w: StakerWallets = self.env.read(&self.env.staker_wallets(&c.staker));
            if !w.hashes().contains(&[n; 32]) {
                return Err(format!(
                    "wallet {n} on record for {} but not in its list",
                    c.staker
                ));
            }
        }
        Ok(())
    }

    fn assets(&self) -> u64 {
        self.env.vault_liquid() + self.pool().deployed_book
    }

    /// The books after every step.
    fn check(
        &mut self,
        name: &str,
        ok: bool,
        did: &Did,
        pre: &Pool,
        pre_assets: u64,
    ) -> Result<(), String> {
        self.check_covered_wallets()?;
        let p = self.pool();
        let now = self.env.now;
        let liquid = self.env.vault_liquid();
        let reserved = p.staker_yield_reserved + p.backer_yield_reserved;
        // Unpaid yield is pool cash (claims may spend it); the rounding reserve never is.
        if liquid < p.rounding_reserve {
            return Err(format!(
                "rounding reserve not in cash: liquid {liquid} < reserve {}",
                p.rounding_reserve
            ));
        }

        // Watch list: every place from head to tail exists, the one before the head is closed.
        for seq in p.watch_head..p.watch_tail {
            if !self.env.exists(&self.env.watch_entry(seq)) {
                return Err(format!("watch place {seq} missing"));
            }
        }
        if p.watch_head > 0 && self.env.exists(&self.env.watch_entry(p.watch_head - 1)) {
            return Err("watch place behind the head not closed".into());
        }

        // Stakes: live records sum to total_staked exactly (no loss path), count exact.
        let (mut sum, mut count) = (0u64, 0u64);
        for i in 0..STAKERS {
            if let Some(r) = self.stake_rec(i) {
                if !r.forfeited {
                    sum += r.amount;
                    count += (r.amount > 0) as u64;
                }
                if r.forfeited && !r.claim_approved {
                    return Err("forfeited stake without an approved claim".into());
                }
                if r.version != 1 {
                    return Err(format!("stake record version {}", r.version));
                }
            }
        }
        if sum != p.total_staked || count != p.total_stakers {
            return Err(format!(
                "stakes: records {sum}/{count}, pool {}/{}",
                p.total_staked, p.total_stakers
            ));
        }
        // Backing: exact, no loss path touches it. Open withdrawals: exact, never above the backing.
        let (mut backed, mut pending, mut open) = (0u64, 0u64, 0u64);
        for r in self.all_backer_recs() {
            backed += r.amount;
            pending += r.pending[..r.pending_count as usize]
                .iter()
                .map(|d| d.amount)
                .sum::<u64>();
            let unpaid = r.withdraw_amount - r.withdraw_paid;
            open += unpaid;
            if unpaid > r.amount {
                return Err(format!("backer withdrawal {unpaid} > backing {}", r.amount));
            }
        }
        if backed != p.total_backed || pending != p.total_backed_pending {
            return Err(format!(
                "backing: records {backed}/{pending}, pool {}/{}",
                p.total_backed, p.total_backed_pending
            ));
        }
        if open != p.total_backer_open {
            return Err(format!(
                "backer open: records {open}, pool {}",
                p.total_backer_open
            ));
        }
        // The line: every place from head to tail exists and names its own claim's place.
        if p.queue_head > p.queue_tail {
            return Err(format!(
                "queue head {} > tail {}",
                p.queue_head, p.queue_tail
            ));
        }
        for seq in p.queue_head..p.queue_tail {
            let entry = self.env.queue_entry(seq);
            if !self.env.exists(&entry) {
                return Err(format!("queue place {seq} missing"));
            }
            let claim: Claim = self.env.read(&self.env.queue_state(seq).claim);
            if claim.queue_seq != seq {
                return Err(format!(
                    "queue place {seq} names a claim at {}",
                    claim.queue_seq
                ));
            }
        }
        // Claims: allocation equals the unpaid part of every admitted open claim.
        let mut allocated = 0u64;
        for c in self.claims.clone() {
            if let Some(cl) = self.claim(&c) {
                if cl.streamed > cl.entitlement {
                    return Err(format!(
                        "claim streamed {} > entitlement {}",
                        cl.streamed, cl.entitlement
                    ));
                }
                if matches!(
                    cl.status,
                    ClaimStatus::PendingTime | ClaimStatus::AwaitingApproval | ClaimStatus::Active
                ) {
                    allocated += cl.entitlement - cl.streamed;
                }
            }
        }
        if allocated != p.total_allocated {
            return Err(format!(
                "allocated: claims {allocated}, pool {}",
                p.total_allocated
            ));
        }
        // Holdings vs books; only paid claims may leave the books short.
        let assets = liquid + p.deployed_book;
        let owed = p.total_staked
            + p.total_backed
            + p.total_backed_pending
            + reserved
            + p.protocol_yield_balance;
        let gap = owed.saturating_sub(assets);
        self.max_gap = self.max_gap.max(gap);
        if gap > self.paid {
            return Err(format!(
                "books {owed} > held {assets} + claims paid {}",
                self.paid
            ));
        }
        // Settings stay in range and in order.
        let st = p.settings();
        for key in SettingKey::ALL {
            st.check_value(key, st.get(key))
                .map_err(|e| format!("setting {key:?} = {} invalid: {e:?}", st.get(key)))?;
        }

        let was_paused = pre.is_paused(now);
        if ok {
            // Only the two exits move money out while paused: a staker's emergency exit and a
            // backer's completed withdrawal ("works while paused", the backer's emergency exit).
            let exit = matches!(did, Did::Emergency | Did::CapitalOut { staker: None, .. });
            if was_paused && !exit && assets < pre_assets {
                return Err(format!(
                    "{name} moved money while paused: {pre_assets} -> {assets}"
                ));
            }
            match did {
                Did::CapitalOut {
                    staker,
                    amount,
                    beneficiary,
                    before,
                } => {
                    let cap = pool_core::capacity(p.total_staked, p.total_backed).unwrap();
                    if p.total_allocated > cap {
                        return Err(format!(
                            "{name}: open claims {} > capacity {cap} after exit",
                            p.total_allocated
                        ));
                    }
                    if let Some(i) = staker {
                        let left = self.stake_rec(*i).map(|r| r.amount).unwrap_or(0);
                        let (min, _) = pool_core::stake::bounds(&st).unwrap();
                        if left != 0 && left < min {
                            return Err(format!("dust stake {left} < min {min}"));
                        }
                    }
                    // Paid exactly the principal, in USDC, to the right account.
                    let got = self.env.usdc(beneficiary).saturating_sub(*before);
                    if got != *amount {
                        return Err(format!("{name} paid {got} for {amount}"));
                    }
                }
                Did::Pause => {
                    let gap = pre.settings().get(SettingKey::PauseGapSecs);
                    if pre.is_paused(now) || (pre.paused_until != 0 && now < pre.paused_until + gap)
                    {
                        return Err("pause allowed inside the gap or while paused".into());
                    }
                }
                Did::Expire | Did::WithdrawYield if was_paused => {
                    return Err(format!("{name} went through while paused"));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Largest books-over-holdings gap of one episode.
fn run_episode(seed: u64, stats: &mut BTreeMap<&'static str, (u64, u64)>) -> Result<u64, String> {
    let mut w = World::new(seed);
    for i in 0..STEPS {
        let pre = w.pool();
        let pre_assets = w.assets();
        let (name, ok, did) = w.step();
        let e = stats.entry(name).or_default();
        e.0 += 1;
        e.1 += ok as u64;
        w.trace.push(format!(
            "{i:>3} {name} {}",
            if ok { "ok" } else { "refused" }
        ));
        if std::env::var("FUZZ_VERBOSE").is_ok() {
            let p = w.pool();
            let owed = p.total_staked
                + p.total_backed
                + p.total_backed_pending
                + p.staker_yield_reserved
                + p.backer_yield_reserved
                + p.protocol_yield_balance;
            let held = w.assets();
            println!(
                "{i:>3} {name:<28} {} held {held} owed {owed} diff {} | liquid {} book {} staked {} backed {} pend {} yS {} yB {} prot {} alloc {} paused {}",
                ok as u8, held as i128 - owed as i128, w.env.vault_liquid(), p.deployed_book,
                p.total_staked, p.total_backed, p.total_backed_pending,
                p.staker_yield_reserved, p.backer_yield_reserved, p.protocol_yield_balance, p.total_allocated,
                p.is_paused(w.env.now) as u8
            );
        }
        if let Err(msg) = w.check(name, ok, &did, &pre, pre_assets) {
            let tail = w.trace[w.trace.len().saturating_sub(40)..].join("\n");
            return Err(format!("seed {seed} step {i}: {msg}\n{tail}"));
        }
    }
    Ok(w.max_gap)
}

#[test]
#[ignore]
fn fuzz_pool() {
    let secs: u64 = std::env::var("FUZZ_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    let episodes: u64 = std::env::var("FUZZ_EPISODES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(u64::MAX);
    let base: u64 = std::env::var("FUZZ_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos() as u64
        });
    println!("fuzz: base seed {base}, {secs}s, demo build {DEMO_BUILD}");
    let end = Instant::now() + Duration::from_secs(secs);
    let mut stats = BTreeMap::new();
    let (mut n, mut max_gap) = (0u64, 0u64);
    let mut failure = None;
    while Instant::now() < end && n < episodes {
        match run_episode(base.wrapping_add(n), &mut stats) {
            Ok(g) => max_gap = max_gap.max(g),
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
        n += 1;
    }
    println!(
        "episodes {n}, steps {}, largest books-over-holdings gap {max_gap} USDC base units",
        n * STEPS as u64
    );
    for (name, (tried, went)) in &stats {
        println!("  {name:<28} tried {tried:>7}  went through {went:>7}");
    }
    if let Some(e) = failure {
        panic!("{e}");
    }
}
