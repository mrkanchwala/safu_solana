//! Stateful program fuzz: random sequences of every liquidity path (stake, partial and whole
//! withdraw, emergency exit, backing and its withdrawal, claims end to end, yield, harvest,
//! rebalance, treasury, pause, settings through the timelock, Marinade rewards and liquidity)
//! against the real program and Marinade in LiteSVM. Ordinary refusals are expected and ignored;
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
use safu_pool::state::{BackerRecord, Claim, ClaimStatus, Pool, StakeRecord};
use solana_keypair::Keypair;
use solana_signer::Signer;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const STEPS: usize = 250;
const STAKERS: usize = 4;
const BACKERS: usize = 3;

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
    /// Marinade loss marked off `total_staked` but left in the stake records (finding F1): it
    /// lets the books run short as well, so it is counted and allowed, not failed.
    unassigned_loss: u64,
    last_unassigned: u64,
    /// Every Marinade loss the pool booked, from its `DeploymentLoss` events. With no stakers left
    /// to mark it off (`total_staked` saturates at zero) it shows only here.
    marinade_loss: u64,
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
            let kp = env.funded(200 * SOL);
            stakers.push(Staker {
                kp,
                beneficiary: Keypair::new().pubkey(),
            });
        }
        let backers = (0..BACKERS).map(|_| env.funded(500 * SOL)).collect();
        World {
            env,
            rng: Rng(seed.max(1)),
            stakers,
            backers,
            claims: Vec::new(),
            next_tx: 1,
            paid: 0,
            unassigned_loss: 0,
            last_unassigned: 0,
            marinade_loss: 0,
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
        let a = self.env.backer_record(&self.backers[i].pubkey());
        self.env.exists(&a).then(|| self.env.read(&a))
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
        // Counts any Marinade loss the pool booked (`DeploymentLoss` events).
        use anchor_lang::Discriminator;
        use base64::Engine;
        let Ok(logs) = self.env.send_logs(ixs, &[who]) else {
            return false;
        };
        let disc = safu_pool::events::DeploymentLoss::DISCRIMINATOR;
        for line in logs {
            let Some(b64) = line.strip_prefix("Program data: ") else {
                continue;
            };
            let Ok(d) = base64::engine::general_purpose::STANDARD.decode(b64) else {
                continue;
            };
            if d.len() >= disc.len() + 8 && &d[..disc.len()] == disc {
                let at = disc.len();
                self.marinade_loss += u64::from_le_bytes(d[at..at + 8].try_into().unwrap());
            }
        }
        true
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
                let before = self.env.lamports(&ben);
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
                let a = self.rng.range(1, 60 * SOL);
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
                let amount = self.backer_rec(bi).map(|r| r.withdraw_amount).unwrap_or(0);
                let before = self.env.lamports(&bpk);
                let ix = self.env.complete_backer_ix(&bpk);
                let ok = self.send(&[ix], &bk);
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
                let ratio = [TIER_A_RATIO, TIER_B_RATIO, TIER_C_RATIO][tier as usize % 3];
                let top = rec.amount.saturating_mul(ratio).max(1);
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
            16 => match self.open_claim() {
                Some(c) => (
                    "try_release_queued_claim",
                    self.transition(c, safu_pool::instruction::TryReleaseQueuedClaim {}),
                    Did::Other,
                ),
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
            23 => {
                let ix = if self.rng.chance(50) {
                    self.env.upkeep_ix(safu_pool::instruction::Harvest {})
                } else {
                    self.env.upkeep_ix(safu_pool::instruction::Rebalance {})
                };
                ("harvest_or_rebalance", self.send(&[ix], &admin), Did::Other)
            }
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
            26 | 38 => {
                let ix = self.env.admin_ix(safu_pool::instruction::Unpause {});
                ("unpause", self.send(&[ix], &admin), Did::Other)
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
            29 => {
                match self.rng.below(3) {
                    0 => self.env.marinade_rewards(self.rng.range(1, 30)),
                    1 => self.env.marinade_liquidity_at_target(),
                    _ => {
                        let l = self.rng.range(0, 5_000 * SOL);
                        self.env.set_marinade_liquidity(l)
                    }
                }
                ("marinade_moves", true, Did::Other)
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
        pre_msol: u64,
    ) -> Result<(), String> {
        let p = self.pool();
        let now = self.env.now;
        let liquid = self.env.vault_liquid();
        let reserved = p.staker_yield_reserved + p.backer_yield_reserved;
        if liquid < reserved {
            return Err(format!(
                "set-aside yield not in cash: liquid {liquid} < reserved {reserved}"
            ));
        }
        let msol = self.env.pool_msol_amount();
        if msol < p.deployed_msol {
            return Err(format!(
                "mSOL on the books {} > held {msol}",
                p.deployed_msol
            ));
        }

        // Stakes: live records carry at least total_staked (a Marinade loss marks it down), and
        // the staker count is exact.
        let (mut sum, mut count) = (0u64, 0u64);
        for i in 0..STAKERS {
            if let Some(r) = self.stake_rec(i) {
                if !r.forfeited {
                    sum += r.amount;
                    count += 1;
                }
                if r.version != 1 {
                    return Err(format!("stake record version {}", r.version));
                }
            }
        }
        if sum < p.total_staked || count != p.total_stakers {
            return Err(format!(
                "stakes: records {sum}/{count}, pool {}/{}",
                p.total_staked, p.total_stakers
            ));
        }
        // Backing: exact, no loss path touches it.
        let (mut backed, mut pending) = (0u64, 0u64);
        for i in 0..BACKERS {
            if let Some(r) = self.backer_rec(i) {
                backed += r.amount;
                pending += r.pending_amount;
                if r.withdraw_amount > r.amount {
                    return Err(format!(
                        "backer withdrawal {} > backing {}",
                        r.withdraw_amount, r.amount
                    ));
                }
            }
        }
        if backed != p.total_backed || pending != p.total_backed_pending {
            return Err(format!(
                "backing: records {backed}/{pending}, pool {}/{}",
                p.total_backed, p.total_backed_pending
            ));
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
        let unassigned = sum.saturating_sub(p.total_staked);
        self.unassigned_loss += unassigned.saturating_sub(self.last_unassigned);
        self.last_unassigned = unassigned;
        let gap = owed.saturating_sub(assets);
        self.max_gap = self.max_gap.max(gap);
        if gap > self.paid + self.marinade_loss {
            return Err(format!(
                "books {owed} > held {assets} + claims paid {} + Marinade loss {}",
                self.paid, self.marinade_loss
            ));
        }
        // Every loss that left the stake records full was booked as a Marinade loss.
        if self.unassigned_loss > self.marinade_loss {
            return Err(format!(
                "stake records exceed total_staked by {} with only {} Marinade loss booked",
                self.unassigned_loss, self.marinade_loss
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
            if was_paused && !exit && (assets < pre_assets || msol < pre_msol) {
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
                    // Paid at least the principal less Marinade's own max fee.
                    // (Stakers only: their payout goes to a separate address; a backer pays
                    // its own transaction fee.)
                    let got = self.env.lamports(beneficiary).saturating_sub(*before);
                    let floor = amount - amount * lp_max_fee_bps(&self.env) / 10_000;
                    if staker.is_some() && got < floor {
                        return Err(format!("{name} paid {got} < floor {floor} for {amount}"));
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

fn lp_max_fee_bps(env: &Env) -> u64 {
    let d = env.svm.get_account(&env.cfg.marinade_state).unwrap().data;
    let o = pool_core::marinade::STATE_LP_MAX_FEE_BPS;
    u64::from(u32::from_le_bytes(d[o..o + 4].try_into().unwrap()))
}

/// (largest gap, unassigned loss) of one episode.
fn run_episode(
    seed: u64,
    stats: &mut BTreeMap<&'static str, (u64, u64)>,
) -> Result<(u64, u64), String> {
    let mut w = World::new(seed);
    for i in 0..STEPS {
        let pre = w.pool();
        let pre_assets = w.assets();
        let pre_msol = w.env.pool_msol_amount();
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
                "{i:>3} {name:<28} {} held {held} owed {owed} diff {} | liquid {} book {} msol {}/{} staked {} backed {} pend {} yS {} yB {} prot {} alloc {} paused {}",
                ok as u8, held as i128 - owed as i128, w.env.vault_liquid(), p.deployed_book, p.deployed_msol,
                w.env.pool_msol_amount(), p.total_staked, p.total_backed, p.total_backed_pending,
                p.staker_yield_reserved, p.backer_yield_reserved, p.protocol_yield_balance, p.total_allocated,
                p.is_paused(w.env.now) as u8
            );
        }
        if let Err(msg) = w.check(name, ok, &did, &pre, pre_assets, pre_msol) {
            let tail = w.trace[w.trace.len().saturating_sub(40)..].join("\n");
            return Err(format!("seed {seed} step {i}: {msg}\n{tail}"));
        }
    }
    Ok((w.max_gap, w.marinade_loss))
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
    let (mut n, mut max_gap, mut loss_eps, mut first_loss) = (0u64, 0u64, 0u64, None);
    let mut failure = None;
    while Instant::now() < end && n < episodes {
        match run_episode(base.wrapping_add(n), &mut stats) {
            Ok((g, loss)) => {
                max_gap = max_gap.max(g);
                if loss > 0 {
                    loss_eps += 1;
                    first_loss.get_or_insert(base.wrapping_add(n));
                }
            }
            Err(e) => {
                failure = Some(e);
                break;
            }
        }
        n += 1;
    }
    println!(
        "episodes {n}, steps {}, largest books-over-holdings gap {max_gap} lamports",
        n * STEPS as u64
    );
    println!("F1 Marinade loss booked in {loss_eps} episodes (first seed {first_loss:?})");
    for (name, (tried, went)) in &stats {
        println!("  {name:<28} tried {tried:>7}  went through {went:>7}");
    }
    if let Some(e) = failure {
        panic!("{e}");
    }
}
