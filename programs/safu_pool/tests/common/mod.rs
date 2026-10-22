//! LiteSVM harness shared by every test file. USDC is a 6-decimal classic SPL Token mint created per
//! test; token accounts are written straight into the bank at their associated-token addresses.
//! Deploy values come from `config/pool.devnet.json`, rule numbers from `pool_core::params`:
//! nothing is retyped here.
#![allow(dead_code)]

use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::{
    prelude::{Clock, Pubkey},
    solana_program::instruction::{AccountMeta, Instruction},
    AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas,
};
use litesvm::LiteSVM;
pub use pool_core::settings::SettingKey;
use safu_pool::approval::{approval_hash, encode_message, ClaimApproval};
use safu_pool::constants::*;
// Setting defaults (a fresh pool's live values). Tests that change a setting read `Env::setting`.
// Each test file uses a different subset.
#[allow(unused_imports)]
pub use pool_core::params::{
    APPROVE_WINDOW_SECS, BACKER_MATURITY_SECS, BACKER_NOTICE_SECS, BACKER_YIELD_BPS,
    COLLECTION_INACTIVITY_SECS, COOLDOWN_SECS, MAX_STAKE_BPS, MIN_STAKE_BPS, PAUSE_GAP_SECS,
    PAUSE_MAX_SECS, STAKER_YIELD_BPS, VESTING_SECS,
};
use safu_pool::state::{BackerRecord, Claim, Pool, StakeRecord};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{Message, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;

/// Lamports in one SOL: transaction fees and rent only. Every pool amount is USDC.
pub const SOL: u64 = 1_000_000_000;
/// One USDC in base units, from the one decimals definition (`pool_core::params::USDC_DECIMALS`).
pub const USDC: u64 = 10u64.pow(pool_core::params::USDC_DECIMALS as u32);
/// Any fixed start time; every test moves the clock from here.
pub const START: i64 = 1_800_000_000;
pub const SYSTEM: Pubkey = solana_system_interface::program::ID;
pub const TOKEN: Pubkey = spl_token_interface::ID;
pub const ATA_PROGRAM: Pubkey =
    anchor_lang::prelude::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub const ED25519: Pubkey =
    anchor_lang::prelude::pubkey!("Ed25519SigVerify111111111111111111111111111");
pub const IX_SYSVAR: Pubkey =
    anchor_lang::prelude::pubkey!("Sysvar1nstructions1111111111111111111111111");

const CONFIG: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../config/pool.devnet.json");

pub struct Config {
    pub pool_cap: u64,
    pub compute_unit_limit: u64,
}

pub fn config() -> Config {
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(CONFIG).unwrap()).unwrap();
    Config {
        pool_cap: v["poolCapUsdc"].as_u64().unwrap(),
        compute_unit_limit: v["computeUnitLimit"].as_u64().unwrap(),
    }
}

/// SPL Token `Mint` (82 bytes): no mint authority needed in tests, the harness writes balances.
pub fn mint_data(decimals: u8, supply: u64) -> Vec<u8> {
    let mut d = vec![0u8; 82];
    d[36..44].copy_from_slice(&supply.to_le_bytes());
    d[44] = decimals;
    d[45] = 1; // initialized
    d
}

/// SPL Token `Account` (165 bytes), initialized, no delegate, not native.
pub fn token_data(mint: &Pubkey, owner: &Pubkey, amount: u64) -> Vec<u8> {
    let mut d = vec![0u8; 165];
    d[..32].copy_from_slice(mint.as_ref());
    d[32..64].copy_from_slice(owner.as_ref());
    d[64..72].copy_from_slice(&amount.to_le_bytes());
    d[108] = 1; // AccountState::Initialized
    d
}

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
    assert_eq!(
        &account.data[..4],
        &3u32.to_le_bytes(),
        "not a ProgramData account"
    );
    account.data[12] = 1;
    account.data[13..45].copy_from_slice(authority.as_ref());
    svm.set_account(address, account).unwrap();
}

pub struct Env {
    pub svm: LiteSVM,
    pub cfg: Config,
    /// This test's USDC mint.
    pub usdc_mint: Pubkey,
    pub admin: Keypair,
    pub co_signer: Keypair,
    pub guardian: Keypair,
    pub oracle: Keypair,
    pub writer: Keypair,
    pub treasury: Keypair,
    pub now: i64,
    /// The yield venue's accounts, once a test sets one up (`tests/kamino.rs`); every money call
    /// passes them.
    pub venue: Option<Venue>,
}

/// A klend reserve's accounts, as the pool's calls pass them.
#[derive(Clone, Copy, Debug)]
pub struct Venue {
    pub reserve: Pubkey,
    pub market: Pubkey,
    pub authority: Pubkey,
    pub supply: Pubkey,
    pub collateral_mint: Pubkey,
    pub pool_collateral: Pubkey,
    pub oracle: Pubkey,
}

impl Env {
    /// Program and USDC mint loaded, clock set, roles funded; pool not initialized.
    pub fn uninitialized() -> Env {
        Env::uninitialized_at(Keypair::new().pubkey(), START)
    }

    /// `uninitialized` with the USDC mint at `usdc_mint` and the clock at `start` (a mainnet fork
    /// needs the real mint and a time its price feeds know).
    pub fn uninitialized_at(usdc_mint: Pubkey, start: i64) -> Env {
        let mut svm = LiteSVM::new();
        let cfg = config();
        svm.add_program(
            safu_pool::ID,
            include_bytes!("../../../../target/deploy/safu_pool.so"),
        )
        .unwrap();
        set_token_program_account(
            &mut svm,
            usdc_mint,
            mint_data(pool_core::params::USDC_DECIMALS, 0),
        );
        let mut clock: Clock = svm.get_sysvar();
        clock.unix_timestamp = start;
        svm.set_sysvar(&clock);
        let (admin, co_signer, guardian, oracle, writer, treasury) = (
            Keypair::new(),
            Keypair::new(),
            Keypair::new(),
            Keypair::new(),
            Keypair::new(),
            Keypair::new(),
        );
        for k in [&admin, &co_signer, &guardian, &oracle, &writer, &treasury] {
            svm.airdrop(&k.pubkey(), 100 * SOL).unwrap();
        }
        set_upgrade_authority(&mut svm, &safu_pool::ID, &admin.pubkey());
        let mut env = Env {
            svm,
            cfg,
            usdc_mint,
            admin,
            co_signer,
            guardian,
            oracle,
            writer,
            treasury,
            now: start,
            venue: None,
        };
        let t = env.treasury.pubkey();
        env.open_usdc(&t);
        env
    }

    /// Initialized with the devnet config.
    pub fn new() -> Env {
        Env::new_at(Keypair::new().pubkey(), START)
    }

    pub fn new_at(usdc_mint: Pubkey, start: i64) -> Env {
        let mut env = Env::uninitialized_at(usdc_mint, start);
        let args = env.init_args();
        let admin = env.admin.insecure_clone();
        let ix = env.init_ix(&admin.pubkey(), args, env.usdc_mint);
        env.ok(&[ix], &[&admin]);
        env
    }

    pub fn init_args(&self) -> safu_pool::instructions::InitArgs {
        safu_pool::instructions::InitArgs {
            co_signer: self.co_signer.pubkey(),
            guardian: self.guardian.pubkey(),
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
        usdc_mint: Pubkey,
    ) -> Instruction {
        let pool = self.pool();
        self.ix(
            safu_pool::accounts::Initialize {
                admin: *admin,
                program: safu_pool::ID,
                program_data: programdata(&safu_pool::ID),
                pool,
                usdc_mint,
                vault: self.vault(),
                governance: self.governance(),
                token_program: TOKEN,
                system_program: SYSTEM,
            },
            safu_pool::instruction::Initialize { args },
        )
    }

    pub fn ix(&self, accounts: impl ToAccountMetas, data: impl InstructionData) -> Instruction {
        Instruction {
            program_id: safu_pool::ID,
            accounts: accounts.to_account_metas(None),
            data: data.data(),
        }
    }

    // ---- addresses

    pub fn pool(&self) -> Pubkey {
        pda(&[SEED_POOL])
    }
    pub fn vault(&self) -> Pubkey {
        pda(&[SEED_VAULT, self.pool().as_ref()])
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
    pub fn governance(&self) -> Pubkey {
        pda(&[SEED_GOVERNANCE, self.pool().as_ref()])
    }
    /// The pending change in governance slot `kind` (zero-copy account: read in place).
    pub fn gov_pending(&self, kind: u8) -> safu_pool::state::GovProposal {
        let data = self.svm.get_account(&self.governance()).unwrap().data;
        let size = std::mem::size_of::<safu_pool::state::GovProposal>();
        let at = 8 + kind as usize * size;
        *bytemuck::from_bytes(&data[at..at + size])
    }
    /// `owner`'s USDC account: the associated-token address, as wallets use.
    pub fn ata(&self, owner: &Pubkey) -> Pubkey {
        Pubkey::find_program_address(
            &[owner.as_ref(), TOKEN.as_ref(), self.usdc_mint.as_ref()],
            &ATA_PROGRAM,
        )
        .0
    }
    /// The mint, the token program, and the venue's accounts (or none).
    pub fn cash(&self) -> safu_pool::accounts::Cash {
        let (usdc_mint, token_program) = (self.usdc_mint, TOKEN);
        match self.venue {
            None => safu_pool::accounts::Cash {
                usdc_mint,
                token_program,
                klend_program: None,
                reserve: None,
                lending_market: None,
                lending_market_authority: None,
                reserve_liquidity_supply: None,
                reserve_collateral_mint: None,
                pool_collateral: None,
                oracle: None,
                instructions: None,
            },
            Some(v) => safu_pool::accounts::Cash {
                usdc_mint,
                token_program,
                klend_program: Some(safu_pool::constants::KLEND_PROGRAM_ID),
                reserve: Some(v.reserve),
                lending_market: Some(v.market),
                lending_market_authority: Some(v.authority),
                reserve_liquidity_supply: Some(v.supply),
                reserve_collateral_mint: Some(v.collateral_mint),
                pool_collateral: Some(v.pool_collateral),
                oracle: Some(v.oracle),
                instructions: Some(IX_SYSVAR),
            },
        }
    }

    // ---- sending

    pub fn send(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> Result<(), String> {
        self.send_cu(ixs, signers).map(|_| ())
    }

    /// Sends at the site's compute limit and returns the compute units the transaction used.
    pub fn send_cu(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> Result<u64, String> {
        self.send_at_limit(ixs, signers)
    }

    /// Sends with upkeep but no compute limit instruction: Solana's 200k default, as any other
    /// caller might.
    pub fn send_default_limit(
        &mut self,
        ixs: &[Instruction],
        signers: &[&Keypair],
    ) -> Result<u64, String> {
        let ixs = self.with_upkeep(ixs);
        self.send_bare(&ixs, signers)
    }

    /// As `send_cu`, with the compute limit the site asks for (`computeUnitLimit` in the config).
    pub fn send_at_limit(
        &mut self,
        ixs: &[Instruction],
        signers: &[&Keypair],
    ) -> Result<u64, String> {
        let mut ixs = self.with_upkeep(ixs);
        // ComputeBudget `SetComputeUnitLimit` (tag 2, u32 units). Last, so instruction indexes the
        // pool reads (approval signatures) stay where they are.
        let mut data = vec![2u8];
        data.extend_from_slice(&(self.cfg.compute_unit_limit as u32).to_le_bytes());
        ixs.push(Instruction {
            program_id: solana_sdk_ids::compute_budget::ID,
            accounts: vec![],
            data,
        });
        self.send_bare(&ixs, signers)
    }

    /// Sends exactly these instructions, without the upkeep accounts.
    pub fn send_bare(&mut self, ixs: &[Instruction], signers: &[&Keypair]) -> Result<u64, String> {
        let msg = Message::new_with_blockhash(
            ixs,
            Some(&signers[0].pubkey()),
            &self.svm.latest_blockhash(),
        );
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), signers).unwrap();
        let r = self
            .svm
            .send_transaction(tx)
            .map(|m| m.compute_units_consumed)
            .map_err(|e| format!("{:?} | logs: {:?}", e.err, e.meta.logs));
        self.svm.expire_blockhash();
        r
    }

    /// Sends and returns the transaction logs (events are `Program data:` lines).
    pub fn send_logs(
        &mut self,
        ixs: &[Instruction],
        signers: &[&Keypair],
    ) -> Result<Vec<String>, String> {
        let ixs = self.with_upkeep(ixs);
        let msg = Message::new_with_blockhash(
            &ixs,
            Some(&signers[0].pubkey()),
            &self.svm.latest_blockhash(),
        );
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), signers).unwrap();
        let r = self
            .svm
            .send_transaction(tx)
            .map(|m| m.logs)
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

    /// A new wallet with `usdc` USDC in its USDC account and SOL for fees and rent.
    pub fn funded(&mut self, usdc: u64) -> Keypair {
        let k = Keypair::new();
        self.svm.airdrop(&k.pubkey(), 10 * SOL).unwrap();
        self.set_usdc(&k.pubkey(), usdc);
        k
    }

    /// Writes `owner`'s USDC balance (creating its account if needed).
    pub fn set_usdc(&mut self, owner: &Pubkey, amount: u64) {
        let (mint, ata) = (self.usdc_mint, self.ata(owner));
        set_token_program_account(&mut self.svm, ata, token_data(&mint, owner, amount));
    }

    /// Opens an empty USDC account for `owner` if it has none (a fresh beneficiary).
    pub fn open_usdc(&mut self, owner: &Pubkey) {
        if !self.exists(&self.ata(owner)) {
            self.set_usdc(owner, 0);
        }
    }

    /// USDC in `owner`'s USDC account (0 if it has none).
    pub fn usdc(&self, owner: &Pubkey) -> u64 {
        self.token_amount(&self.ata(owner))
    }

    /// Adds `amount` to a token account's balance in place (a donation, or yield arriving).
    pub fn add_token(&mut self, account: &Pubkey, amount: u64) {
        let mut a = self.svm.get_account(account).expect("token account exists");
        let now = u64::from_le_bytes(a.data[64..72].try_into().unwrap());
        a.data[64..72].copy_from_slice(&(now + amount).to_le_bytes());
        self.svm.set_account(*account, a).unwrap();
    }

    pub fn token_amount(&self, account: &Pubkey) -> u64 {
        self.svm
            .get_account(account)
            .map(|a| u64::from_le_bytes(a.data[64..72].try_into().unwrap()))
            .unwrap_or(0)
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
    pub fn edit<T: AccountDeserialize + AccountSerialize>(
        &mut self,
        a: &Pubkey,
        f: impl FnOnce(&mut T),
    ) {
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

    /// USDC in the vault.
    pub fn vault_liquid(&self) -> u64 {
        self.token_amount(&self.vault())
    }

    // ---- instruction builders

    pub fn admin_ix(&self, data: impl InstructionData) -> Instruction {
        self.ix(
            safu_pool::accounts::AdminOnly {
                admin: self.admin.pubkey(),
                pool: self.pool(),
            },
            data,
        )
    }

    /// Passes the staker's backer record only when `amount` is above the live max stake (the
    /// overflow becomes backing), as a client would.
    pub fn stake_ix(&self, staker: &Pubkey, amount: u64, beneficiary: Pubkey) -> Instruction {
        let max = pool_core::stake::bounds(&self.pool_state().settings())
            .unwrap()
            .1;
        let record = (amount > max).then(|| self.backer_record(staker));
        self.stake_ix_with(staker, amount, beneficiary, record)
    }
    pub fn stake_ix_with(
        &self,
        staker: &Pubkey,
        amount: u64,
        beneficiary: Pubkey,
        backer_record: Option<Pubkey>,
    ) -> Instruction {
        self.ix(
            safu_pool::accounts::Stake {
                staker: *staker,
                pool: self.pool(),
                vault: self.vault(),
                stake_record: self.stake_record(staker),
                staker_usdc: self.ata(staker),
                backer_record,
                system_program: SYSTEM,
                cash: self.cash(),
            },
            safu_pool::instruction::Stake {
                amount,
                beneficiary,
            },
        )
    }

    /// The whole stake (whatever the record holds; 1 lamport if there is no record, for error tests).
    pub fn withdraw_ix(&self, staker: &Pubkey, beneficiary: &Pubkey) -> Instruction {
        self.withdraw_part_ix(staker, beneficiary, self.stake_amount(staker))
    }

    pub fn withdraw_part_ix(
        &self,
        staker: &Pubkey,
        beneficiary: &Pubkey,
        amount: u64,
    ) -> Instruction {
        self.ix(
            safu_pool::accounts::Withdraw {
                staker: *staker,
                pool: self.pool(),
                vault: self.vault(),
                stake_record: self.stake_record(staker),
                beneficiary_usdc: self.ata(beneficiary),
                cash: self.cash(),
            },
            safu_pool::instruction::Withdraw { amount },
        )
    }

    /// The whole stake (see `withdraw_ix`).
    pub fn emergency_exit_ix(&self, staker: &Pubkey) -> Instruction {
        self.emergency_exit_part_ix(staker, self.stake_amount(staker))
    }

    pub fn emergency_exit_part_ix(&self, staker: &Pubkey, amount: u64) -> Instruction {
        self.ix(
            safu_pool::accounts::EmergencyExit {
                staker: *staker,
                pool: self.pool(),
                vault: self.vault(),
                stake_record: self.stake_record(staker),
                staker_usdc: self.ata(staker),
                cash: self.cash(),
            },
            safu_pool::instruction::EmergencyExit { amount },
        )
    }

    pub fn set_beneficiary_ix(&self, staker: &Pubkey, beneficiary: Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::SetBeneficiary {
                staker: *staker,
                pool: self.pool(),
                stake_record: self.stake_record(staker),
            },
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
                backer_usdc: self.ata(backer),
                system_program: SYSTEM,
                cash: self.cash(),
            },
            safu_pool::instruction::Back { amount },
        )
    }

    pub fn mature_ix(&self, backer: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::MatureBacking {
                pool: self.pool(),
                backer_record: self.backer_record(backer),
                vault: self.vault(),
                cash: self.cash(),
            },
            safu_pool::instruction::MatureBacking {},
        )
    }

    pub fn request_backer_ix(&self, backer: &Pubkey, amount: u64) -> Instruction {
        self.ix(
            safu_pool::accounts::RequestBackerWithdrawal {
                backer: *backer,
                pool: self.pool(),
                backer_record: self.backer_record(backer),
            },
            safu_pool::instruction::RequestBackerWithdrawal { amount },
        )
    }

    pub fn backer_only_ix(&self, backer: &Pubkey, data: impl InstructionData) -> Instruction {
        self.ix(
            safu_pool::accounts::BackerOnly {
                backer: *backer,
                pool: self.pool(),
                backer_record: self.backer_record(backer),
            },
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
                backer_usdc: self.ata(backer),
                cash: self.cash(),
            },
            safu_pool::instruction::CompleteBackerWithdrawal {},
        )
    }

    pub fn register_ix(
        &self,
        writer: &Pubkey,
        staker: Pubkey,
        wallet_hash: [u8; 32],
    ) -> Instruction {
        self.ix(
            safu_pool::accounts::RegisterWallet {
                registry_writer: *writer,
                pool: self.pool(),
                staker_wallets: self.staker_wallets(&staker),
                covered_wallet: self.covered(&wallet_hash),
                system_program: SYSTEM,
            },
            safu_pool::instruction::RegisterWallet {
                staker,
                wallet_hash,
            },
        )
    }

    // ---- claims

    pub fn claim_addr(&self, staker: &Pubkey, tx: &[u8; 32]) -> Pubkey {
        pda(&[
            SEED_CLAIM,
            self.pool().as_ref(),
            staker.as_ref(),
            tx.as_ref(),
        ])
    }
    pub fn override_addr(&self, claim: &Pubkey) -> Pubkey {
        pda(&[SEED_OVERRIDE, claim.as_ref()])
    }
    pub fn claim_state(&self, staker: &Pubkey, tx: &[u8; 32]) -> Claim {
        self.read(&self.claim_addr(staker, tx))
    }

    /// An approval signed "now", valid for the longest allowed window, hack "now" (never before a stake made this second).
    pub fn approval(
        &self,
        staker: &Pubkey,
        tx: [u8; 32],
        entitlement: u64,
        tier: u8,
    ) -> ClaimApproval {
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
        pda(&[
            SEED_REVOKED,
            self.pool().as_ref(),
            approval_hash(&self.message(a)).as_ref(),
        ])
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
                queue_entry: self.queue_entry(self.pool_state().queue_tail),
                watch_entry: self.watch_entry(self.pool_state().watch_tail),
                system_program: SYSTEM,
            },
            safu_pool::instruction::SubmitClaim {
                approval: a.clone(),
            },
        )
    }

    /// Precompile + submit, signed by the oracle.
    pub fn submit(&mut self, a: &ClaimApproval) -> Result<(), String> {
        let oracle = self.oracle.insecure_clone();
        let ed = ed25519_ix(&oracle, &self.message(a), [0, 0, 0]);
        let ix = self.submit_ix(&oracle.pubkey(), a);
        self.send(&[ed, ix], &[&oracle])
    }

    pub fn transition_ix(
        &self,
        staker: &Pubkey,
        tx: &[u8; 32],
        data: impl InstructionData,
    ) -> Instruction {
        self.ix(
            safu_pool::accounts::ClaimTransition {
                pool: self.pool(),
                claim: self.claim_addr(staker, tx),
                stake_record: self.stake_record(staker),
            },
            data,
        )
    }

    // ---- the claim line
    pub fn queue_entry(&self, seq: u64) -> Pubkey {
        pda(&[SEED_QUEUE, self.pool().as_ref(), &seq.to_le_bytes()])
    }
    pub fn queue_state(&self, seq: u64) -> safu_pool::state::QueueEntry {
        self.read(&self.queue_entry(seq))
    }
    pub fn watch_entry(&self, seq: u64) -> Pubkey {
        pda(&[SEED_WATCH, self.pool().as_ref(), &seq.to_le_bytes()])
    }
    pub fn watch_state(&self, seq: u64) -> safu_pool::state::WatchEntry {
        self.read(&self.watch_entry(seq))
    }

    /// The upkeep accounts a user transaction passes now: the head of the line, then the watch
    /// places, each as (place, claim, stake record, rent payer). Empty while paused.
    pub fn upkeep_metas(&self) -> Vec<AccountMeta> {
        let mut out = Vec::new();
        if !self.exists(&self.pool()) {
            return out;
        }
        let p = self.pool_state();
        if p.is_paused(self.now) {
            return out;
        }
        let mut place = |entry: Pubkey, claim: Pubkey, payer: Pubkey| {
            let staker = self.read::<Claim>(&claim).staker;
            for a in [entry, claim, self.stake_record(&staker), payer] {
                out.push(AccountMeta::new(a, false));
            }
        };
        if p.queue_head < p.queue_tail {
            let e = self.queue_state(p.queue_head);
            place(self.queue_entry(p.queue_head), e.claim, e.payer);
        }
        let watch = pool_core::upkeep::watch_places(p.watch_head, p.watch_tail, p.watch_cursor);
        for pos in watch.into_iter().flatten() {
            let e = self.watch_state(pos);
            place(self.watch_entry(pos), e.claim, e.payer);
        }
        out
    }

    /// Every pool instruction carries the upkeep accounts, as a client sends them.
    fn with_upkeep(&self, ixs: &[Instruction]) -> Vec<Instruction> {
        let metas = self.upkeep_metas();
        ixs.iter()
            .cloned()
            .map(|mut ix| {
                if ix.program_id == safu_pool::ID {
                    ix.accounts.extend(metas.iter().cloned());
                }
                ix
            })
            .collect()
    }
    /// The place of `staker`'s claim on `tx` and who gets its rent back.
    fn place_of(&self, staker: &Pubkey, tx: &[u8; 32]) -> (Pubkey, Pubkey) {
        let seq = self.claim_state(staker, tx).queue_seq;
        let entry = self.queue_entry(seq);
        let payer = if self.exists(&entry) {
            self.queue_state(seq).payer
        } else {
            self.oracle.pubkey()
        };
        (entry, payer)
    }
    pub fn release_ix(&self, staker: &Pubkey, tx: &[u8; 32]) -> Instruction {
        let (queue_entry, rent_to) = self.place_of(staker, tx);
        self.ix(
            safu_pool::accounts::ReleaseQueuedClaim {
                pool: self.pool(),
                claim: self.claim_addr(staker, tx),
                queue_entry,
                stake_record: self.stake_record(staker),
                rent_to,
            },
            safu_pool::instruction::TryReleaseQueuedClaim {},
        )
    }
    /// Passes the stake only if its record still exists.
    pub fn advance_ix(&self, staker: &Pubkey, tx: &[u8; 32]) -> Instruction {
        let (queue_entry, rent_to) = self.place_of(staker, tx);
        let stake = self.stake_record(staker);
        self.ix(
            safu_pool::accounts::AdvanceQueue {
                pool: self.pool(),
                claim: self.claim_addr(staker, tx),
                queue_entry,
                stake_record: self.exists(&stake).then_some(stake),
                rent_to,
            },
            safu_pool::instruction::AdvanceQueue {},
        )
    }
    /// Anyone (a fresh fee payer) releases `staker`'s queued claim.
    pub fn release(&mut self, staker: &Pubkey, tx: &[u8; 32]) -> Result<(), String> {
        let payer = self.funded(0);
        let ix = self.release_ix(staker, tx);
        self.send(&[ix], &[&payer])
    }
    pub fn advance(&mut self, staker: &Pubkey, tx: &[u8; 32]) -> Result<(), String> {
        let payer = self.funded(0);
        let ix = self.advance_ix(staker, tx);
        self.send(&[ix], &[&payer])
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
                beneficiary_usdc: self.ata(beneficiary),
                cash: self.cash(),
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
                cash: self.cash(),
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
            self.ix(
                accounts,
                safu_pool::instruction::SuspendStake { staker: *staker },
            )
        } else {
            self.ix(
                accounts,
                safu_pool::instruction::UnsuspendStake { staker: *staker },
            )
        }
    }

    /// The admin plus `approver` (co-signer or guardian) start or extend a suspension.
    pub fn renew_suspension_ix(&self, staker: &Pubkey, approver: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::RenewSuspension {
                admin: self.admin.pubkey(),
                approver: *approver,
                pool: self.pool(),
                stake_record: self.stake_record(staker),
            },
            safu_pool::instruction::RenewSuspension { staker: *staker },
        )
    }

    pub fn revoke_ix(&self, admin: &Pubkey, a: &ClaimApproval, hash: [u8; 32]) -> Instruction {
        self.ix(
            safu_pool::accounts::RevokeApproval {
                admin: *admin,
                pool: self.pool(),
                revoked: pda(&[SEED_REVOKED, self.pool().as_ref(), hash.as_ref()]),
                system_program: SYSTEM,
            },
            safu_pool::instruction::RevokeApproval {
                approval: a.clone(),
                hash,
            },
        )
    }

    pub fn override_ix(
        &self,
        signer: &Pubkey,
        staker: &Pubkey,
        tx: [u8; 32],
        entitlement: u64,
        tier: u8,
    ) -> Instruction {
        let claim = self.claim_addr(staker, &tx);
        self.ix(
            safu_pool::accounts::ApproveOverride {
                signer: *signer,
                pool: self.pool(),
                stake_record: self.stake_record(staker),
                claim,
                override_request: self.override_addr(&claim),
                watch_entry: self.watch_entry(self.pool_state().watch_tail),
                system_program: SYSTEM,
            },
            safu_pool::instruction::ApproveOverride {
                staker: *staker,
                tx_hash: tx,
                entitlement,
                tier,
            },
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

    /// Counted backer money of `amount`: capacity for claims. Waits out the live maturity first if
    /// it is above zero.
    pub fn matured_backer(&mut self, amount: u64) -> Keypair {
        let b = self.funded(amount);
        let ix = self.back_ix(&b.pubkey(), amount);
        self.ok(&[ix], &[&b]);
        let maturity = self.setting(SettingKey::BackerMaturitySecs);
        if maturity > 0 {
            self.warp(maturity);
            let m = self.mature_ix(&b.pubkey());
            self.ok(&[m], &[&b]);
        }
        b
    }

    pub fn claim_yield_ix(&self, staker: &Pubkey, beneficiary: &Pubkey) -> Instruction {
        self.ix(
            safu_pool::accounts::ClaimYield {
                staker: *staker,
                pool: self.pool(),
                vault: self.vault(),
                stake_record: self.stake_record(staker),
                beneficiary_usdc: self.ata(beneficiary),
                cash: self.cash(),
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
                backer_usdc: self.ata(backer),
                cash: self.cash(),
            },
            safu_pool::instruction::ClaimBackerYield {},
        )
    }

    pub fn fund_reserve_ix(&self, funder: &Pubkey, amount: u64) -> Instruction {
        self.ix(
            safu_pool::accounts::FundRoundingReserve {
                funder: *funder,
                pool: self.pool(),
                vault: self.vault(),
                funder_usdc: self.ata(funder),
                cash: self.cash(),
            },
            safu_pool::instruction::FundRoundingReserve { amount },
        )
    }

    /// Runs a view instruction by simulation and decodes its return data.
    pub fn view<T: anchor_lang::AnchorDeserialize>(&mut self, ix: Instruction) -> T {
        let payer = self.admin.insecure_clone();
        let msg =
            Message::new_with_blockhash(&[ix], Some(&payer.pubkey()), &self.svm.latest_blockhash());
        let tx = VersionedTransaction::try_new(VersionedMessage::Legacy(msg), &[&payer]).unwrap();
        let r = self
            .svm
            .simulate_transaction(tx)
            .map_err(|e| format!("{:?}", e.err))
            .unwrap();
        T::deserialize(&mut &r.meta.return_data.data[..]).unwrap()
    }

    pub fn view_staker(&mut self, staker: &Pubkey) -> safu_pool::instructions::StakerView {
        let ix = self.ix(
            safu_pool::accounts::ViewStaker {
                pool: self.pool(),
                stake_record: self.stake_record(staker),
                reserve: self.venue.map(|v| v.reserve),
            },
            safu_pool::instruction::ViewStaker {},
        );
        self.view(ix)
    }

    pub fn view_backer(&mut self, backer: &Pubkey) -> safu_pool::instructions::BackerView {
        let ix = self.ix(
            safu_pool::accounts::ViewBacker {
                pool: self.pool(),
                backer_record: self.backer_record(backer),
                reserve: self.venue.map(|v| v.reserve),
            },
            safu_pool::instruction::ViewBacker {},
        );
        self.view(ix)
    }

    pub fn withdraw_yield_ix(&self, admin: &Pubkey, treasury: &Pubkey, amount: u64) -> Instruction {
        self.ix(
            safu_pool::accounts::WithdrawYield {
                admin: *admin,
                pool: self.pool(),
                vault: self.vault(),
                treasury_usdc: self.ata(treasury),
                cash: self.cash(),
            },
            safu_pool::instruction::WithdrawYield { amount },
        )
    }

    pub fn pause(&mut self) {
        let admin = self.admin.insecure_clone();
        let ix = self.admin_ix(safu_pool::instruction::Pause {});
        self.ok(&[ix], &[&admin]);
    }

    pub fn unpause(&mut self) {
        let admin = self.admin.insecure_clone();
        let ix = self.admin_ix(safu_pool::instruction::Unpause {});
        self.ok(&[ix], &[&admin]);
    }

    /// Principal in a staker's record, or 1 if there is none.
    pub fn stake_amount(&self, staker: &Pubkey) -> u64 {
        if self.exists(&self.stake_record(staker)) {
            self.stake_state(staker).amount
        } else {
            1
        }
    }

    // ---- settings

    pub fn setting(&self, key: SettingKey) -> i64 {
        self.pool_state().settings[key.index()]
    }
    /// The live time gate and claim window (a new stake takes these).
    pub fn time_gate(&self) -> i64 {
        self.setting(SettingKey::TimeGateSecs)
    }
    pub fn claim_window(&self) -> i64 {
        self.setting(SettingKey::ClaimWindowSecs)
    }
    /// Tier ceiling of a new stake of `amount` under the live tier ratios.
    pub fn tier_cap(&self, amount: u64, tier: u8) -> u64 {
        let ratios = self.pool_state().settings().tier_ratios();
        pool_core::claim::tier_cap(amount, tier, ratios).unwrap()
    }

    // ---- governance
    pub fn gov_ix(&self, caller: &Pubkey, data: impl anchor_lang::InstructionData) -> Instruction {
        self.ix(
            safu_pool::accounts::GovCall {
                caller: *caller,
                pool: self.pool(),
                governance: self.governance(),
            },
            data,
        )
    }
    /// `new_key`: the key a role change installs, signing its own installation.
    pub fn execute_change_ix(&self, kind: u8, new_key: Option<Pubkey>) -> Instruction {
        self.ix(
            safu_pool::accounts::GovExecute {
                pool: self.pool(),
                governance: self.governance(),
                new_key,
            },
            safu_pool::instruction::ExecuteChange { kind },
        )
    }
    pub fn propose_ix(&self, key: u8, value: i64) -> Instruction {
        self.admin_ix(safu_pool::instruction::ProposeSetting { key, value })
    }
    pub fn approve_setting_ix(&self, signer: &Pubkey, key: u8, value: i64) -> Instruction {
        self.ix(
            safu_pool::accounts::ApproveSetting {
                co_signer: *signer,
                pool: self.pool(),
            },
            safu_pool::instruction::ApproveSetting { key, value },
        )
    }
    pub fn execute_setting_ix(&self, key: u8) -> Instruction {
        self.ix(
            safu_pool::accounts::ExecuteSetting { pool: self.pool() },
            safu_pool::instruction::ExecuteSetting { key },
        )
    }
    pub fn cancel_setting_ix(&self, signer: &Pubkey, key: u8) -> Instruction {
        self.ix(
            safu_pool::accounts::CancelSetting {
                signer: *signer,
                pool: self.pool(),
            },
            safu_pool::instruction::CancelSetting { key },
        )
    }
    /// The full public path: propose, co-sign, wait out the timelock, execute.
    pub fn set_setting(&mut self, key: SettingKey, value: i64) {
        let (admin, co) = (self.admin.insecure_clone(), self.co_signer.insecure_clone());
        let k = key as u8;
        let ix = self.propose_ix(k, value);
        self.ok(&[ix], &[&admin]);
        let ix = self.approve_setting_ix(&co.pubkey(), k, value);
        self.ok(&[ix], &[&co]);
        self.warp(pool_core::params::SETTINGS_TIMELOCK_SECS);
        // Anyone may execute; the admin just pays the fee here.
        let ix = self.execute_setting_ix(k);
        self.ok(&[ix], &[&admin]);
    }

    /// A funded staker with a live stake of `amount` (beneficiary = itself).
    pub fn staker(&mut self, amount: u64) -> Keypair {
        let k = self.funded(amount);
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
    for v in [
        sig_off,
        indexes[0],
        pk_off,
        indexes[1],
        msg_off,
        message.len() as u16,
        indexes[2],
    ] {
        data.extend_from_slice(&v.to_le_bytes());
    }
    data.extend_from_slice(signature.as_ref());
    data.extend_from_slice(signer.pubkey().as_ref());
    data.extend_from_slice(message);
    Instruction {
        program_id: ED25519,
        accounts: vec![],
        data,
    }
}

/// An admin hold on a stake, in force (`on`) or lifted, as `suspend_stake` / `unsuspend_stake`
/// leave it, without the clocks those set.
pub fn hold(r: &mut StakeRecord, on: bool) {
    r.suspended = on;
    r.suspended_until = if on { i64::MAX } else { 0 };
}

pub fn assert_err(result: Result<(), String>, error: safu_pool::errors::PoolError) {
    let code = anchor_lang::error::ERROR_CODE_OFFSET + error as u32;
    let err = result.expect_err("transaction should have failed");
    assert!(
        err.contains(&format!("Custom({code})")),
        "expected Custom({code}) ({error:?}), got: {err}"
    );
}

/// Stake bounds for the configured pool cap at the default settings, from the same rule the program
/// uses.
pub fn bounds() -> (u64, u64) {
    pool_core::stake::bounds(&pool_core::settings::Settings::defaults(config().pool_cap).unwrap())
        .unwrap()
}

/// The tests' money unit: twice the largest stake the default settings allow (the same ratio the
/// tests were written at). Every pool-sized amount in the tests is a multiple of it, so changing the
/// pool cap or the stake percentages rescales the tests instead of breaking them.
pub fn unit() -> u64 {
    2 * bounds().1
}

/// Writes an account owned by the SPL Token program (mint or token account), rent-exempt.
pub fn set_token_program_account(svm: &mut LiteSVM, key: Pubkey, data: Vec<u8>) {
    let lamports = svm.minimum_balance_for_rent_exemption(data.len());
    svm.set_account(
        key,
        Account {
            lamports,
            data,
            owner: TOKEN,
            executable: false,
            rent_epoch: 0,
        },
    )
    .unwrap();
}
