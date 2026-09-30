//! LiteSVM harness shared by every test file. Marinade runs as the real program, loaded with its
//! devnet accounts from `tests/fixtures/` (dumped 2026-09-30). Deploy values come from
//! `config/pool.devnet.json`, rule numbers from `pool_core::params`: nothing is retyped here.
#![allow(dead_code)]

use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::{
    prelude::{Clock, Pubkey},
    solana_program::instruction::Instruction,
    AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas,
};
use base64::Engine;
use litesvm::LiteSVM;
use safu_pool::constants::*;
use safu_pool::approval::{approval_hash, encode_message, ClaimApproval};
use safu_pool::state::{BackerRecord, Claim, Pool, StakeRecord};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::str::FromStr;

pub const SOL: u64 = 1_000_000_000;
/// Any fixed start time; every test moves the clock from here.
pub const START: i64 = 1_800_000_000;
pub const SYSTEM: Pubkey = solana_system_interface::program::ID;
pub const TOKEN: Pubkey = spl_token_interface::ID;
pub const ED25519: Pubkey = anchor_lang::prelude::pubkey!("Ed25519SigVerify111111111111111111111111111");
pub const IX_SYSVAR: Pubkey = anchor_lang::prelude::pubkey!("Sysvar1nstructions1111111111111111111111111");

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
const CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/pool.devnet.json");

pub struct Config {
    pub pool_cap: u64,
    pub compute_unit_limit: u64,
    pub marinade_program: Pubkey,
    pub marinade_state: Pubkey,
    pub msol_mint: Pubkey,
}

pub fn config() -> Config {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(CONFIG).unwrap()).unwrap();
    let key = |s: &serde_json::Value| Pubkey::from_str(s.as_str().unwrap()).unwrap();
    Config {
        pool_cap: v["poolCapLamports"].as_u64().unwrap(),
        compute_unit_limit: v["computeUnitLimit"].as_u64().unwrap(),
        marinade_program: key(&v["marinade"]["program"]),
        marinade_state: key(&v["marinade"]["state"]),
        msol_mint: key(&v["marinade"]["msolMint"]),
    }
}

/// Loads a `solana account --output json` dump.
fn load_fixture(svm: &mut LiteSVM, name: &str) -> Pubkey {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(format!("{FIXTURES}/{name}.json")).unwrap()).unwrap();
    let a = &v["account"];
    let data = base64::engine::general_purpose::STANDARD.decode(a["data"][0].as_str().unwrap()).unwrap();
    let key = Pubkey::from_str(v["pubkey"].as_str().unwrap()).unwrap();
    svm.set_account(
        key,
        Account {
            lamports: a["lamports"].as_u64().unwrap(),
            data,
            owner: Pubkey::from_str(a["owner"].as_str().unwrap()).unwrap(),
            executable: a["executable"].as_bool().unwrap(),
            rent_epoch: 0,
        },
    )
    .unwrap();
    key
}

pub const MARINADE_FIXTURES: [&str; 8] = [
    "marinade_state",
    "marinade_msol_mint",
    "marinade_liq_sol_leg",
    "marinade_msol_leg",
    "marinade_reserve",
    "marinade_treasury_msol",
    "marinade_st_mint_auth",
    "marinade_msol_leg_auth",
];

pub fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &safu_pool::ID).0
}

pub fn programdata(program_id: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[program_id.as_ref()], &bpf_loader_upgradeable::ID).0
}

/// LiteSVM loads programs with no upgrade authority; set it as a real deploy would.
pub fn set_upgrade_authority(svm: &mut LiteSVM, program_id: &Pubkey, authority: &Pubkey) {
    let address = programdata(program_id);
    let mut account = svm.get_account(&address).expect("program data account");
    // bincode UpgradeableLoaderState::ProgramData: u32 tag (3) · u64 slot · Option<Pubkey> (1 + 32).
    assert_eq!(&account.data[..4], &3u32.to_le_bytes(), "not a ProgramData account");
    account.data[12] = 1;
    account.data[13..45].copy_from_slice(authority.as_ref());
    svm.set_account(address, account).unwrap();
}

pub struct Env {
    pub svm: LiteSVM,
    pub cfg: Config,
    pub admin: Keypair,
    pub co_signer: Keypair,
    pub oracle: Keypair,
    pub writer: Keypair,
    pub treasury: Keypair,
    pub now: i64,
}

impl Env {
    /// Programs and Marinade loaded, clock set, roles funded; pool not initialized.
    pub fn uninitialized() -> Env {
        let mut svm = LiteSVM::new();
        let cfg = config();
        svm.add_program(safu_pool::ID, include_bytes!("../../../../target/deploy/safu_pool.so")).unwrap();
        svm.add_program(cfg.marinade_program, &std::fs::read(format!("{FIXTURES}/marinade.so")).unwrap())
            .unwrap();
        for f in MARINADE_FIXTURES {
            load_fixture(&mut svm, f);
        }
        let mut clock: Clock = svm.get_sysvar();
        clock.unix_timestamp = START;
        svm.set_sysvar(&clock);
        let (admin, co_signer, oracle, writer, treasury) =
            (Keypair::new(), Keypair::new(), Keypair::new(), Keypair::new(), Keypair::new());
        for k in [&admin, &co_signer, &oracle, &writer, &treasury] {
            svm.airdrop(&k.pubkey(), 100 * SOL).unwrap();
        }
        set_upgrade_authority(&mut svm, &safu_pool::ID, &admin.pubkey());
        Env { svm, cfg, admin, co_signer, oracle, writer, treasury, now: START }
    }

    /// Initialized with the devnet config.
    pub fn new() -> Env {
        let mut env = Env::uninitialized();
        let args = env.init_args();
        let admin = env.admin.insecure_clone();
        let ix = env.init_ix(&admin.pubkey(), args, env.cfg.marinade_state, env.cfg.msol_mint);
        env.ok(&[ix], &[&admin]);
        env
    }

    pub fn init_args(&self) -> safu_pool::instructions::InitArgs {
        safu_pool::instructions::InitArgs {
            co_signer: self.co_signer.pubkey(),
            oracle: self.oracle.pubkey(),
            registry_writer: self.writer.pubkey(),
            treasury: self.treasury.pubkey(),
            cluster: CLUSTER_LOCALNET,
            pool_cap: self.cfg.pool_cap,
        }
    }

    pub fn init_ix(
        &self,
        admin: &Pubkey,
        args: safu_pool::instructions::InitArgs,
        marinade_state: Pubkey,
        msol_mint: Pubkey,
    ) -> Instruction {
        let pool = self.pool();
        self.ix(
            safu_pool::accounts::Initialize {
                admin: *admin,
                program: safu_pool::ID,
                program_data: programdata(&safu_pool::ID),
                pool,
                vault: self.vault(),
                marinade_program: self.cfg.marinade_program,
                marinade_state,
                msol_mint,
                pool_msol: self.pool_msol(),
                token_program: TOKEN,
                system_program: SYSTEM,
            },
            safu_pool::instruction::Initialize { args },
        )
    }

    pub fn ix(&self, accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
        Instruction { program_id: safu_pool::ID, accounts: accounts.to_account_metas(None), data: data.data() }
    }

    // ---- addresses

    pub fn pool(&self) -> Pubkey {
        pda(&[SEED_POOL])
    }
    pub fn vault(&self) -> Pubkey {
        pda(&[SEED_VAULT, self.pool().as_ref()])
    }
    /// Marinade accounts for the pool's calls: PDAs from the configured state with the seeds in
    /// `pool_core::marinade`, the mSOL leg and treasury read from the state account itself.
    pub fn leg(&self) -> safu_pool::accounts::MarinadeLeg {
        use pool_core::marinade as m;
        let state = self.cfg.marinade_state;
        let marinade_pda = |seed: &[u8]| Pubkey::find_program_address(&[state.as_ref(), seed], &self.cfg.marinade_program).0;
        let data = self.svm.get_account(&state).expect("marinade state").data;
        let at = |o: usize| Pubkey::try_from(&data[o..o + 32]).unwrap();
        safu_pool::accounts::MarinadeLeg {
            marinade_program: self.cfg.marinade_program,
            marinade_state: state,
            msol_mint: self.cfg.msol_mint,
            liq_pool_sol_leg: marinade_pda(m::SEED_LIQ_POOL_SOL_LEG),
            liq_pool_msol_leg: at(m::STATE_LIQ_POOL_MSOL_LEG),
            liq_pool_msol_leg_authority: marinade_pda(m::SEED_LIQ_POOL_MSOL_LEG_AUTHORITY),
            reserve: marinade_pda(m::SEED_RESERVE),
            msol_mint_authority: marinade_pda(m::SEED_MSOL_MINT_AUTHORITY),
            treasury_msol: at(m::STATE_TREASURY_MSOL),
            pool_msol: self.pool_msol(),
            token_program: TOKEN,
            system_program: SYSTEM,
        }
    }

    pub fn pool_msol(&self) -> Pubkey {
        pda(&[SEED_MSOL, self.pool().as_ref()])
    }
    pub fn stake_record(&self, staker: &Pubkey) -> Pubkey {
        pda(&[SEED_STAKE, self.pool().as_ref(), staker.as_ref()])
    }
    pub fn backer_record(&self, backer: &Pubkey) -> Pubkey {
        pda(&[SEED_BACKER, self.pool().as_ref(), backer.as_ref()])
    }
    pub fn staker_wallets(&self, staker: &Pubkey) -> Pubkey {
        pda(&[SEED_STAKER_WALLETS, self.pool().as_ref(), staker.as_ref()])
    }
    pub fn covered(&self, hash: &[u8; 32]) -> Pubkey {
        pda(&[SEED_COVERED, self.pool().as_ref(), hash.as_ref()])
    }

    // ---- sending

    pub fn send(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), String> {
        self.send_cu(ixs, signers).map(|_| ())
    }

    /// Sends and returns the compute units the transaction used.
    pub fn send_cu(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> Result<u64, String> {
        let msg = Message::new_with_blockhash(ixs, Some(&signers[0].pubkey()), &self.svm.latest_blockhash());
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), signers).unwrap();
        let r = self
            .svm
            .send_transaction(tx)
            .map(|m| m.compute_units_consumed)
            .map_err(|e| format!("{:?} | logs: {:?}", e.err, e.meta.logs));
        self.svm.expire_blockhash();
        r
    }

    pub fn ok(&mut self, ixs: &[Instruction], signers: &[&Keypair]) {
        if let Err(e) = self.send(ixs, signers) {
            panic!("transaction should have succeeded: {e}");
        }
    }

    pub fn warp(&mut self, secs: i64) {
        self.now += secs;
        let mut c: Clock = self.svm.get_sysvar();
        c.unix_timestamp = self.now;
        self.svm.set_sysvar(&c);
        self.svm.expire_blockhash();
    }

    pub fn funded(&mut self, lamports: u64) -> Keypair {
        let k = Keypair::new();
        self.svm.airdrop(&k.pubkey(), lamports).unwrap();
        k
    }

    pub fn lamports(&self, a: &Pubkey) -> u64 {
        self.svm.get_account(a).map(|a| a.lamports).unwrap_or(0)
    }

    pub fn exists(&self, a: &Pubkey) -> bool {
        self.svm.get_account(a).is_some_and(|a| a.lamports > 0)
    }

    // ---- reading and editing accounts

    pub fn read<T: AccountDeserialize>(&self, a: &Pubkey) -> T {
        let acc = self.svm.get_account(a).expect("account exists");
        T::try_deserialize(&mut acc.data.as_slice()).unwrap()
    }

    /// Rewrites an account's data in place (test setup only, for states another step would create).
    pub fn edit<T: AccountDeserialize + AccountSerialize>(&mut self, a: &Pubkey, f: impl FnOnce(&mut T)) {
        let mut acc = self.svm.get_account(a).expect("account exists");
        let mut v = T::try_deserialize(&mut acc.data.as_slice()).unwrap();
        f(&mut v);
        let mut data = Vec::with_capacity(acc.data.len());
        v.try_serialize(&mut data).unwrap();
        data.resize(acc.data.len(), 0);
        acc.data = data;
        self.svm.set_account(*a, acc).unwrap();
    }

    pub fn pool_state(&self) -> Pool {
        self.read(&self.pool())
    }
    pub fn stake_state(&self, staker: &Pubkey) -> StakeRecord {
        self.read(&self.stake_record(staker))
    }
    pub fn backer_state(&self, backer: &Pubkey) -> BackerRecord {
        self.read(&self.backer_record(backer))
    }

    /// Rent-exempt minimum of a data-less account, as the program computes it.
    pub fn rent_floor(&self) -> u64 {
        self.svm.minimum_balance_for_rent_exemption(0)
    }
    pub fn vault_liquid(&self) -> u64 {
        self.lamports(&self.vault()) - self.rent_floor()
    }

    // ---- instruction builders

    pub fn admin_ix(&self, data: impl InstructionData) -> Instruction {
        self.ix(safu_pool::accounts::AdminOnly { admin: self.admin.pubkey(), pool: self.pool() }, data)
    }

    pub fn stake_ix(&self, staker: &Pubkey, amount: u64, beneficiary: Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::Stake {
                staker: *staker,
                pool: self.pool(),
                vault: self.vault(),
                stake_record: self.stake_record(staker),
                system_program: SYSTEM,
                leg: self.leg(),
            },
            safu_pool::instruction::Stake { amount, beneficiary },
        )
    }

    pub fn withdraw_ix(&self, staker: &Pubkey, beneficiary: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::Withdraw {
                staker: *staker,
                pool: self.pool(),
                vault: self.vault(),
                stake_record: self.stake_record(staker),
                beneficiary: *beneficiary,
                system_program: SYSTEM,
                leg: self.leg(),
            },
            safu_pool::instruction::Withdraw {},
        )
    }

    pub fn emergency_exit_ix(&self, staker: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::EmergencyExit {
                staker: *staker,
                pool: self.pool(),
                vault: self.vault(),
                stake_record: self.stake_record(staker),
                system_program: SYSTEM,
                leg: self.leg(),
            },
            safu_pool::instruction::EmergencyExit {},
        )
    }

    pub fn set_beneficiary_ix(&self, staker: &Pubkey, beneficiary: Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::SetBeneficiary { staker: *staker, pool: self.pool(), stake_record: self.stake_record(staker) },
            safu_pool::instruction::SetBeneficiary { beneficiary },
        )
    }

    pub fn back_ix(&self, backer: &Pubkey, amount: u64) -> Instruction {
        self.ix(
            safu_pool::accounts::Back {
                backer: *backer,
                pool: self.pool(),
                vault: self.vault(),
                backer_record: self.backer_record(backer),
                system_program: SYSTEM,
                leg: self.leg(),
            },
            safu_pool::instruction::Back { amount },
        )
    }

    pub fn mature_ix(&self, backer: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::MatureBacking {
                pool: self.pool(),
                vault: self.vault(),
                backer_record: self.backer_record(backer),
                leg: self.leg(),
            },
            safu_pool::instruction::MatureBacking {},
        )
    }

    pub fn request_backer_ix(&self, backer: &Pubkey, amount: u64) -> Instruction {
        self.ix(
            safu_pool::accounts::RequestBackerWithdrawal {
                backer: *backer,
                pool: self.pool(),
                vault: self.vault(),
                backer_record: self.backer_record(backer),
                leg: self.leg(),
            },
            safu_pool::instruction::RequestBackerWithdrawal { amount },
        )
    }

    pub fn backer_only_ix(&self, backer: &Pubkey, data: impl InstructionData) -> Instruction {
        self.ix(
            safu_pool::accounts::BackerOnly { backer: *backer, pool: self.pool(), backer_record: self.backer_record(backer) },
            data,
        )
    }

    pub fn complete_backer_ix(&self, backer: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::CompleteBackerWithdrawal {
                backer: *backer,
                pool: self.pool(),
                vault: self.vault(),
                backer_record: self.backer_record(backer),
                system_program: SYSTEM,
                leg: self.leg(),
            },
            safu_pool::instruction::CompleteBackerWithdrawal {},
        )
    }

    pub fn register_ix(&self, writer: &Pubkey, staker: Pubkey, wallet_hash: [u8; 32]) -> Instruction {
        self.ix(
            safu_pool::accounts::RegisterWallet {
                registry_writer: *writer,
                pool: self.pool(),
                staker_wallets: self.staker_wallets(&staker),
                covered_wallet: self.covered(&wallet_hash),
                system_program: SYSTEM,
            },
            safu_pool::instruction::RegisterWallet { staker, wallet_hash },
        )
    }

    // ---- claims

    pub fn claim_addr(&self, staker: &Pubkey, tx: &[u8; 32]) -> Pubkey {
        pda(&[SEED_CLAIM, self.pool().as_ref(), staker.as_ref(), tx.as_ref()])
    }
    pub fn override_addr(&self, claim: &Pubkey) -> Pubkey {
        pda(&[SEED_OVERRIDE, claim.as_ref()])
    }
    pub fn claim_state(&self, staker: &Pubkey, tx: &[u8; 32]) -> Claim {
        self.read(&self.claim_addr(staker, tx))
    }

    /// An approval signed "now", valid for the longest allowed window, hack "now" (never before a stake made this second).
    pub fn approval(&self, staker: &Pubkey, tx: [u8; 32], entitlement: u64, tier: u8) -> ClaimApproval {
        ClaimApproval {
            staker: *staker,
            tx_hash: tx,
            entitlement,
            tier,
            hack_timestamp: self.now,
            deadline: self.now + MAX_APPROVAL_WINDOW_SECS,
        }
    }

    pub fn message(&self, a: &ClaimApproval) -> Vec<u8> {
        encode_message(&safu_pool::ID, CLUSTER_LOCALNET, a)
    }

    pub fn revoked_addr(&self, a: &ClaimApproval) -> Pubkey {
        pda(&[SEED_REVOKED, self.pool().as_ref(), approval_hash(&self.message(a)).as_ref()])
    }

    pub fn submit_ix(&self, oracle: &Pubkey, a: &ClaimApproval) -> Instruction {
        self.ix(
            safu_pool::accounts::SubmitClaim {
                oracle: *oracle,
                pool: self.pool(),
                stake_record: self.stake_record(&a.staker),
                claim: self.claim_addr(&a.staker, &a.tx_hash),
                revoked: self.revoked_addr(a),
                instructions: IX_SYSVAR,
                system_program: SYSTEM,
            },
            safu_pool::instruction::SubmitClaim { approval: a.clone() },
        )
    }

    /// Precompile + submit, signed by the oracle.
    pub fn submit(&mut self, a: &ClaimApproval) -> Result<(), String> {
        let oracle = self.oracle.insecure_clone();
        let ed = ed25519_ix(&oracle, &self.message(a), [0, 0, 0]);
        let ix = self.submit_ix(&oracle.pubkey(), a);
        self.send(&[ed, ix], &[&oracle])
    }

    pub fn transition_ix(&self, staker: &Pubkey, tx: &[u8; 32], data: impl InstructionData) -> Instruction {
        self.ix(
            safu_pool::accounts::ClaimTransition {
                pool: self.pool(),
                claim: self.claim_addr(staker, tx),
                stake_record: self.stake_record(staker),
            },
            data,
        )
    }

    pub fn approve_claim_ix(&self, staker: &Pubkey, tx: &[u8; 32]) -> Instruction {
        self.ix(
            safu_pool::accounts::ApproveClaim {
                staker: *staker,
                pool: self.pool(),
                claim: self.claim_addr(staker, tx),
                stake_record: self.stake_record(staker),
            },
            safu_pool::instruction::ApproveClaim {},
        )
    }

    pub fn stream_ix(&self, staker: &Pubkey, tx: &[u8; 32], beneficiary: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::ClaimStream {
                staker: *staker,
                pool: self.pool(),
                vault: self.vault(),
                claim: self.claim_addr(staker, tx),
                stake_record: self.stake_record(staker),
                beneficiary: *beneficiary,
                system_program: SYSTEM,
                leg: self.leg(),
            },
            safu_pool::instruction::ClaimStream {},
        )
    }

    pub fn cancel_claim_ix(&self, admin: &Pubkey, staker: &Pubkey, tx: &[u8; 32]) -> Instruction {
        self.ix(
            safu_pool::accounts::CancelClaim {
                admin: *admin,
                pool: self.pool(),
                claim: self.claim_addr(staker, tx),
                stake_record: self.stake_record(staker),
                vault: self.vault(),
                leg: self.leg(),
            },
            safu_pool::instruction::CancelClaim {},
        )
    }

    pub fn suspend_ix(&self, staker: &Pubkey, claim: Option<Pubkey>, suspend: bool) -> Instruction {
        let accounts = safu_pool::accounts::AdminStake {
            admin: self.admin.pubkey(),
            pool: self.pool(),
            stake_record: self.stake_record(staker),
            claim,
        };
        if suspend {
            self.ix(accounts, safu_pool::instruction::SuspendStake { staker: *staker })
        } else {
            self.ix(accounts, safu_pool::instruction::UnsuspendStake { staker: *staker })
        }
    }

    pub fn revoke_ix(&self, admin: &Pubkey, a: &ClaimApproval, hash: [u8; 32]) -> Instruction {
        self.ix(
            safu_pool::accounts::RevokeApproval {
                admin: *admin,
                pool: self.pool(),
                revoked: pda(&[SEED_REVOKED, self.pool().as_ref(), hash.as_ref()]),
                system_program: SYSTEM,
            },
            safu_pool::instruction::RevokeApproval { approval: a.clone(), hash },
        )
    }

    pub fn override_ix(&self, signer: &Pubkey, staker: &Pubkey, tx: [u8; 32], entitlement: u64, tier: u8) -> Instruction {
        let claim = self.claim_addr(staker, &tx);
        self.ix(
            safu_pool::accounts::ApproveOverride {
                signer: *signer,
                pool: self.pool(),
                stake_record: self.stake_record(staker),
                claim,
                override_request: self.override_addr(&claim),
                system_program: SYSTEM,
            },
            safu_pool::instruction::ApproveOverride { staker: *staker, tx_hash: tx, entitlement, tier },
        )
    }

    pub fn cancel_override_ix(&self, admin: &Pubkey, claim: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::CancelPendingOverride {
                admin: *admin,
                pool: self.pool(),
                override_request: self.override_addr(claim),
            },
            safu_pool::instruction::CancelPendingOverride {},
        )
    }

    /// Matured backer money of `amount`: capacity for claims.
    pub fn matured_backer(&mut self, amount: u64) -> Keypair {
        let b = self.funded(amount + SOL);
        let ix = self.back_ix(&b.pubkey(), amount);
        self.ok(&[ix], &[&b]);
        self.warp(BACKER_MATURITY_SECS);
        let m = self.mature_ix(&b.pubkey());
        self.ok(&[m], &[&b]);
        b
    }

    fn marinade_u64(&self, offset: usize) -> u64 {
        let d = self.svm.get_account(&self.cfg.marinade_state).unwrap().data;
        u64::from_le_bytes(d[offset..offset + 8].try_into().unwrap())
    }

    /// Sets the SOL in Marinade's liquidity pool (above its rent floor).
    pub fn set_marinade_liquidity(&mut self, lamports: u64) {
        let leg = self.leg().liq_pool_sol_leg;
        let mut a = self.svm.get_account(&leg).unwrap();
        a.lamports = lamports + self.rent_floor();
        self.svm.set_account(leg, a).unwrap();
    }

    /// Marinade's liquidity pool filled to its target: the unstake fee is at its minimum (normal
    /// conditions). The devnet dump sits far below target (fee near the maximum).
    pub fn marinade_liquidity_at_target(&mut self) {
        let target = self.marinade_u64(pool_core::marinade::STATE_LP_LIQUIDITY_TARGET);
        self.set_marinade_liquidity(target);
    }

    /// Marinade's fee in bps for unstaking `lamports` now, by its own formula.
    pub fn marinade_fee_bps(&self, lamports: u64) -> u32 {
        use pool_core::marinade as m;
        let d = self.svm.get_account(&self.cfg.marinade_state).unwrap().data;
        let u32_at = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        let available = self.lamports(&self.leg().liq_pool_sol_leg) - self.rent_floor();
        m::unstake_fee_bps(
            available.saturating_sub(lamports),
            self.marinade_u64(m::STATE_LP_LIQUIDITY_TARGET),
            u32_at(m::STATE_LP_MIN_FEE_BPS),
            u32_at(m::STATE_LP_MAX_FEE_BPS),
        )
    }

    pub fn msol_price(&self) -> u64 {
        self.marinade_u64(pool_core::marinade::STATE_MSOL_PRICE)
    }

    /// Staking rewards of `bps` land, as at a Marinade epoch update: the SOL behind all mSOL grows
    /// (added to its reserve balance, which Marinade pays unstakes from) and the cached
    /// `msol_price` is recomputed from it.
    pub fn marinade_rewards(&mut self, bps: u64) {
        use pool_core::marinade as m;
        let supply = self.marinade_u64(m::STATE_MSOL_SUPPLY);
        let price = self.msol_price();
        let behind = pool_core::leg::msol_value(supply, price).unwrap();
        let reward = pool_core::apply_bps(behind, bps).unwrap();
        let reserve = self.marinade_u64(m::STATE_AVAILABLE_RESERVE_BALANCE) + reward;
        let new_price = pool_core::mul_div_floor(behind + reward, m::PRICE_DENOMINATOR as u64, supply).unwrap();
        let key = self.cfg.marinade_state;
        let mut a = self.svm.get_account(&key).unwrap();
        for (o, v) in [(m::STATE_AVAILABLE_RESERVE_BALANCE, reserve), (m::STATE_MSOL_PRICE, new_price)] {
            a.data[o..o + 8].copy_from_slice(&v.to_le_bytes());
        }
        self.svm.set_account(key, a).unwrap();
    }

    /// Most Marinade may keep of `amount` under the pool's unstake fee limit.
    pub fn max_unstake_fee(amount: u64) -> u64 {
        pool_core::apply_bps(amount, pool_core::params::MAX_REBALANCE_SLIPPAGE_BPS).unwrap()
    }

    // ---- B3: Marinade leg and yield

    pub fn upkeep_ix(&self, data: impl InstructionData) -> Instruction {
        self.ix(safu_pool::accounts::Upkeep { pool: self.pool(), vault: self.vault(), leg: self.leg() }, data)
    }

    pub fn claim_yield_ix(&self, staker: &Pubkey, beneficiary: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::ClaimYield {
                staker: *staker,
                pool: self.pool(),
                vault: self.vault(),
                stake_record: self.stake_record(staker),
                beneficiary: *beneficiary,
                leg: self.leg(),
            },
            safu_pool::instruction::ClaimYield {},
        )
    }

    pub fn claim_backer_yield_ix(&self, backer: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::ClaimBackerYield {
                backer: *backer,
                pool: self.pool(),
                vault: self.vault(),
                backer_record: self.backer_record(backer),
                leg: self.leg(),
            },
            safu_pool::instruction::ClaimBackerYield {},
        )
    }

    pub fn withdraw_yield_ix(&self, admin: &Pubkey, treasury: &Pubkey, amount: u64) -> Instruction {
        self.ix(
            safu_pool::accounts::WithdrawYield {
                admin: *admin,
                pool: self.pool(),
                vault: self.vault(),
                treasury: *treasury,
                leg: self.leg(),
            },
            safu_pool::instruction::WithdrawYield { amount },
        )
    }

    /// The pool's mSOL, from its token account.
    pub fn pool_msol_amount(&self) -> u64 {
        let a = self.svm.get_account(&self.pool_msol()).unwrap();
        anchor_spl::token::TokenAccount::try_deserialize(&mut &a.data[..]).unwrap().amount
    }

    pub fn pause(&mut self) {
        let admin = self.admin.insecure_clone();
        let ix = self.admin_ix(safu_pool::instruction::Pause {});
        self.ok(&[ix], &[&admin]);
    }

    /// A funded staker with a live stake of `amount` (beneficiary = itself).
    pub fn staker(&mut self, amount: u64) -> Keypair {
        let k = self.funded(amount + SOL);
        let ix = self.stake_ix(&k.pubkey(), amount, k.pubkey());
        self.ok(&[ix], &[&k]);
        k
    }
}

/// One-signature Ed25519 precompile instruction. `indexes` = [signature, pubkey, message] instruction
/// indexes; a self-contained instruction uses its own position in the transaction.
pub fn ed25519_ix(signer: &Keypair, message: &[u8], indexes: [u16; 3]) -> Instruction {
    let signature = signer.sign_message(message);
    // Header (2) + one offsets record (14), then signature (64), pubkey (32), message.
    let sig_off = 16u16;
    let pk_off = sig_off + 64;
    let msg_off = pk_off + 32;
    let mut data = vec![1u8, 0u8];
    for v in [sig_off, indexes[0], pk_off, indexes[1], msg_off, message.len() as u16, indexes[2]] {
        data.extend_from_slice(&v.to_le_bytes());
    }
    data.extend_from_slice(signature.as_ref());
    data.extend_from_slice(signer.pubkey().as_ref());
    data.extend_from_slice(message);
    Instruction { program_id: ED25519, accounts: vec![], data }
}

pub fn assert_err(result: Result<(), String>, error: safu_pool::errors::PoolError) {
    let code = anchor_lang::error::ERROR_CODE_OFFSET + error as u32;
    let err = result.expect_err("transaction should have failed");
    assert!(err.contains(&format!("Custom({code})")), "expected Custom({code}) ({error:?}), got: {err}");
}

/// Stake bounds for the configured pool cap, from the same rule the program uses.
pub fn bounds() -> (u64, u64) {
    pool_core::stake::bounds(config().pool_cap).unwrap()
}
