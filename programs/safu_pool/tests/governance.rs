//! Key-loss-safe governance: three roles, two to change anything, a public wait, last-resort
//! recovery. One test per rule in `instructions/governance.rs` (multichain `governance.rs`).

mod common;

use anchor_lang::prelude::Pubkey;
use common::*;
use pool_core::params::{GOV_DELAY_SECS, MAX_DEPLOY_BPS, RECOVERY_DELAY_SECS};
use safu_pool::constants::*;
use safu_pool::errors::PoolError;
use safu_pool::instruction::{ApproveChange, CancelChange, ProposeChange, ProposeRecovery};
use safu_pool::state::{GovChange, Pool, ACCOUNT_VERSION};
use solana_keypair::Keypair;
use solana_signer::Signer;

fn propose(env: &mut Env, who: &Keypair, change: GovChange) -> Result<(), String> {
    let ix = env.gov_ix(&who.pubkey(), ProposeChange { change });
    env.send(&[ix], &[who]).map(|_| ())
}
fn approve(env: &mut Env, who: &Keypair, change: GovChange) -> Result<(), String> {
    let ix = env.gov_ix(&who.pubkey(), ApproveChange { change });
    env.send(&[ix], &[who]).map(|_| ())
}
fn recover(env: &mut Env, who: &Keypair, change: GovChange) -> Result<(), String> {
    let ix = env.gov_ix(&who.pubkey(), ProposeRecovery { change });
    env.send(&[ix], &[who]).map(|_| ())
}
fn cancel(env: &mut Env, who: &Keypair, kind: u8) -> Result<(), String> {
    let ix = env.gov_ix(&who.pubkey(), CancelChange { kind });
    env.send(&[ix], &[who]).map(|_| ())
}
/// Anyone executes; `new_key` signs when the change installs a key.
fn execute(env: &mut Env, kind: u8, new_key: Option<&Keypair>) -> Result<(), String> {
    let payer = env.funded(0);
    let ix = env.execute_change_ix(kind, new_key.map(|k| k.pubkey()));
    match new_key {
        Some(k) => env.send(&[ix], &[&payer, k]),
        None => env.send(&[ix], &[&payer]),
    }
    .map(|_| ())
}

struct Roles {
    admin: Keypair,
    co: Keypair,
    guardian: Keypair,
}

/// A pool that has held money: every change waits the full public delay.
fn funded_env() -> (Env, Roles) {
    let mut env = Env::new();
    env.staker(bounds().0);
    assert_ne!(env.pool_state().funded_at, 0);
    let roles = Roles {
        admin: env.admin.insecure_clone(),
        co: env.co_signer.insecure_clone(),
        guardian: env.guardian.insecure_clone(),
    };
    (env, roles)
}

// ------------------------------------------------------------------ standard path

#[test]
fn two_roles_then_the_wait_then_anyone_executes() {
    let (mut env, r) = funded_env();
    let change = GovChange::DeployBps(MAX_DEPLOY_BPS / 2);
    propose(&mut env, &r.admin, change).unwrap();
    // One role alone: no clock yet.
    assert_eq!(env.gov_pending(GOV_KIND_DEPLOY_BPS).eta, 0);
    assert_err(
        execute(&mut env, GOV_KIND_DEPLOY_BPS, None),
        PoolError::GovNotReady,
    );
    approve(&mut env, &r.guardian, change).unwrap();
    assert_eq!(
        env.gov_pending(GOV_KIND_DEPLOY_BPS).eta,
        env.now + GOV_DELAY_SECS
    );
    env.warp(GOV_DELAY_SECS - 1);
    assert_err(
        execute(&mut env, GOV_KIND_DEPLOY_BPS, None),
        PoolError::GovNotReady,
    );
    env.warp(1);
    execute(&mut env, GOV_KIND_DEPLOY_BPS, None).unwrap();
    assert_eq!(env.pool_state().deploy_bps, MAX_DEPLOY_BPS / 2);
    assert_eq!(env.gov_pending(GOV_KIND_DEPLOY_BPS).active, 0);
}

#[test]
fn only_the_three_roles_vote() {
    let (mut env, r) = funded_env();
    let outsider = env.funded(0);
    let change = GovChange::Treasury(Keypair::new().pubkey());
    assert_err(
        propose(&mut env, &outsider, change),
        PoolError::GovCallerNotRole,
    );
    let oracle = env.oracle.insecure_clone();
    assert_err(
        propose(&mut env, &oracle, change),
        PoolError::GovCallerNotRole,
    );
    propose(&mut env, &r.co, change).unwrap();
    assert_err(
        approve(&mut env, &outsider, change),
        PoolError::GovCallerNotRole,
    );
    assert_err(
        approve(&mut env, &r.co, change),
        PoolError::GovAlreadyApproved,
    );
}

#[test]
fn one_pending_change_per_kind_and_approvals_must_match() {
    let (mut env, r) = funded_env();
    let change = GovChange::Treasury(Keypair::new().pubkey());
    propose(&mut env, &r.admin, change).unwrap();
    let other = GovChange::Treasury(Keypair::new().pubkey());
    assert_err(propose(&mut env, &r.co, other), PoolError::GovChangePending);
    assert_err(
        approve(&mut env, &r.co, other),
        PoolError::GovChangeMismatch,
    );
    assert_err(
        approve(&mut env, &r.co, GovChange::DeployBps(1)),
        PoolError::GovNoPendingChange,
    );
}

#[test]
fn a_role_has_no_say_in_its_own_replacement() {
    let (mut env, r) = funded_env();
    let new = Keypair::new();
    let change = GovChange::CoSigner(new.pubkey());
    assert_err(
        propose(&mut env, &r.co, change),
        PoolError::GovTargetCannotApprove,
    );
    propose(&mut env, &r.admin, change).unwrap();
    assert_err(
        approve(&mut env, &r.co, change),
        PoolError::GovTargetCannotApprove,
    );
    assert_err(
        cancel(&mut env, &r.co, GOV_KIND_CO_SIGNER),
        PoolError::GovCannotCancel,
    );
    approve(&mut env, &r.guardian, change).unwrap();
    env.warp(GOV_DELAY_SECS);
    // The new key signs its own installation.
    assert_err(
        execute(&mut env, GOV_KIND_CO_SIGNER, None),
        PoolError::GovNewKeyMustSign,
    );
    let wrong = Keypair::new();
    assert_err(
        execute(&mut env, GOV_KIND_CO_SIGNER, Some(&wrong)),
        PoolError::GovNewKeyMustSign,
    );
    execute(&mut env, GOV_KIND_CO_SIGNER, Some(&new)).unwrap();
    assert_eq!(env.pool_state().co_signer, new.pubkey());
}

#[test]
fn roles_and_oracle_stay_four_different_keys() {
    let (mut env, r) = funded_env();
    let admin = r.admin.pubkey();
    for change in [
        GovChange::Oracle(admin),
        GovChange::Guardian(env.oracle.pubkey()),
        GovChange::CoSigner(r.guardian.pubkey()),
    ] {
        let proposer = if matches!(change, GovChange::Guardian(_)) {
            &r.admin
        } else {
            &r.guardian
        };
        assert_err(
            propose(&mut env, proposer, change),
            PoolError::GovRoleNotDistinct,
        );
    }
}

#[test]
fn a_rotation_voids_the_old_keys_approvals() {
    let (mut env, r) = funded_env();
    // Deploy share approved by admin + co-signer.
    let deploy = GovChange::DeployBps(1);
    propose(&mut env, &r.admin, deploy).unwrap();
    approve(&mut env, &r.co, deploy).unwrap();
    // Meanwhile the co-signer is replaced (admin + guardian).
    let new_co = Keypair::new();
    let rotate = GovChange::CoSigner(new_co.pubkey());
    propose(&mut env, &r.admin, rotate).unwrap();
    approve(&mut env, &r.guardian, rotate).unwrap();
    env.warp(GOV_DELAY_SECS);
    execute(&mut env, GOV_KIND_CO_SIGNER, Some(&new_co)).unwrap();
    // The old co-signer's approval no longer counts: one live approval left.
    assert_err(
        execute(&mut env, GOV_KIND_DEPLOY_BPS, None),
        PoolError::GovNotReady,
    );
}

#[test]
fn cancelling_takes_two_roles() {
    let (mut env, r) = funded_env();
    let change = GovChange::Treasury(Keypair::new().pubkey());
    propose(&mut env, &r.admin, change).unwrap();
    cancel(&mut env, &r.admin, GOV_KIND_TREASURY).unwrap();
    // One vote: still pending.
    assert_eq!(env.gov_pending(GOV_KIND_TREASURY).active, 1);
    assert_err(
        cancel(&mut env, &r.admin, GOV_KIND_TREASURY),
        PoolError::GovAlreadyApproved,
    );
    cancel(&mut env, &r.guardian, GOV_KIND_TREASURY).unwrap();
    assert_eq!(env.gov_pending(GOV_KIND_TREASURY).active, 0);
    assert_err(
        cancel(&mut env, &r.co, GOV_KIND_TREASURY),
        PoolError::GovNoPendingChange,
    );
}

#[test]
fn money_settings_stay_in_range() {
    let (mut env, r) = funded_env();
    assert_err(
        propose(&mut env, &r.admin, GovChange::DeployBps(MAX_DEPLOY_BPS + 1)),
        PoolError::DeployBpsTooHigh,
    );
    // The yield venue cannot move while money sits in it.
    env.edit::<Pool>(&env.pool(), |p| p.deployed_book = 1);
    assert_err(
        propose(
            &mut env,
            &r.admin,
            GovChange::YieldVenue(Keypair::new().pubkey()),
        ),
        PoolError::VenueChangeWhileDeployed,
    );
}

#[test]
fn registry_writer_and_oracle_rotate_with_their_own_signature() {
    let (mut env, r) = funded_env();
    for (kind, new, make) in [
        (
            GOV_KIND_ORACLE,
            Keypair::new(),
            GovChange::Oracle as fn(Pubkey) -> GovChange,
        ),
        (
            GOV_KIND_REGISTRY_WRITER,
            Keypair::new(),
            GovChange::RegistryWriter,
        ),
    ] {
        let change = make(new.pubkey());
        propose(&mut env, &r.admin, change).unwrap();
        approve(&mut env, &r.co, change).unwrap();
        env.warp(GOV_DELAY_SECS);
        assert_err(execute(&mut env, kind, None), PoolError::GovNewKeyMustSign);
        execute(&mut env, kind, Some(&new)).unwrap();
    }
    let p = env.pool_state();
    assert_ne!(p.oracle, env.oracle.pubkey());
    assert_ne!(p.registry_writer, env.writer.pubkey());
}

// ------------------------------------------------------------------ before the first deposit

#[test]
fn setup_changes_need_no_wait_before_any_money_arrives() {
    let mut env = Env::new();
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let treasury = Keypair::new().pubkey();
    let change = GovChange::Treasury(treasury);
    propose(&mut env, &admin, change).unwrap();
    approve(&mut env, &co, change).unwrap();
    execute(&mut env, GOV_KIND_TREASURY, None).unwrap();
    assert_eq!(env.pool_state().treasury, treasury);
    // A role is never a setup change: it always waits.
    let new = Keypair::new();
    let change = GovChange::Guardian(new.pubkey());
    propose(&mut env, &admin, change).unwrap();
    approve(&mut env, &co, change).unwrap();
    assert_err(
        execute(&mut env, GOV_KIND_GUARDIAN, Some(&new)),
        PoolError::GovNotReady,
    );
}

#[test]
fn the_first_deposit_gives_an_approved_setup_change_the_full_wait() {
    let mut env = Env::new();
    let (admin, co) = (env.admin.insecure_clone(), env.co_signer.insecure_clone());
    let change = GovChange::Treasury(Keypair::new().pubkey());
    propose(&mut env, &admin, change).unwrap();
    approve(&mut env, &co, change).unwrap();
    // Money arrives before anyone executes it.
    env.staker(bounds().0);
    let funded_at = env.pool_state().funded_at;
    assert_err(
        execute(&mut env, GOV_KIND_TREASURY, None),
        PoolError::GovNotReady,
    );
    env.warp(funded_at + GOV_DELAY_SECS - env.now);
    execute(&mut env, GOV_KIND_TREASURY, None).unwrap();
    // A later deposit never moves the mark.
    let ix_backer = env.funded(unit());
    let ix = env.back_ix(&ix_backer.pubkey(), unit());
    env.ok(&[ix], &[&ix_backer]);
    assert_eq!(env.pool_state().funded_at, funded_at);
}

// ------------------------------------------------------------------ last-resort recovery

#[test]
fn one_role_alone_replaces_a_lost_role_after_the_long_wait() {
    let (mut env, r) = funded_env();
    let new_admin = Keypair::new();
    let change = GovChange::Admin(new_admin.pubkey());
    recover(&mut env, &r.guardian, change).unwrap();
    let p = env.gov_pending(GOV_KIND_ADMIN);
    assert_eq!((p.solo, p.eta), (1, env.now + RECOVERY_DELAY_SECS));
    // Not an ordinary proposal: no approvals.
    assert_err(approve(&mut env, &r.co, change), PoolError::GovSoloProposal);
    env.warp(RECOVERY_DELAY_SECS - 1);
    assert_err(
        execute(&mut env, GOV_KIND_ADMIN, Some(&new_admin)),
        PoolError::GovNotReady,
    );
    env.warp(1);
    execute(&mut env, GOV_KIND_ADMIN, Some(&new_admin)).unwrap();
    assert_eq!(env.pool_state().admin, new_admin.pubkey());
}

#[test]
fn any_other_working_role_stops_a_recovery_at_once() {
    let (mut env, r) = funded_env();
    let change = GovChange::Admin(Keypair::new().pubkey());
    // Even the role it would replace.
    recover(&mut env, &r.guardian, change).unwrap();
    cancel(&mut env, &r.admin, GOV_KIND_ADMIN).unwrap();
    assert_eq!(env.gov_pending(GOV_KIND_ADMIN).active, 0);
    recover(&mut env, &r.guardian, change).unwrap();
    cancel(&mut env, &r.co, GOV_KIND_ADMIN).unwrap();
    assert_eq!(env.gov_pending(GOV_KIND_ADMIN).active, 0);
}

#[test]
fn recovery_is_only_for_replacing_another_role() {
    let (mut env, r) = funded_env();
    assert_err(
        recover(
            &mut env,
            &r.admin,
            GovChange::Treasury(Keypair::new().pubkey()),
        ),
        PoolError::GovRecoveryRoleOnly,
    );
    assert_err(
        recover(
            &mut env,
            &r.admin,
            GovChange::Admin(Keypair::new().pubkey()),
        ),
        PoolError::GovRecoveryRoleOnly,
    );
}

#[test]
fn a_recovery_lapses_if_its_proposer_loses_the_role() {
    let (mut env, r) = funded_env();
    let new_admin = Keypair::new();
    recover(&mut env, &r.guardian, GovChange::Admin(new_admin.pubkey())).unwrap();
    // The guardian is replaced meanwhile (admin + co-signer).
    let new_guardian = Keypair::new();
    let change = GovChange::Guardian(new_guardian.pubkey());
    propose(&mut env, &r.admin, change).unwrap();
    approve(&mut env, &r.co, change).unwrap();
    env.warp(GOV_DELAY_SECS);
    execute(&mut env, GOV_KIND_GUARDIAN, Some(&new_guardian)).unwrap();
    env.warp(RECOVERY_DELAY_SECS);
    assert_err(
        execute(&mut env, GOV_KIND_ADMIN, Some(&new_admin)),
        PoolError::GovNotReady,
    );
}

#[test]
fn initialize_needs_four_different_role_keys() {
    let mut env = Env::uninitialized();
    let admin = env.admin.insecure_clone();
    let mut args = env.init_args();
    args.guardian = args.oracle;
    let ix = env.init_ix(&admin.pubkey(), args, env.usdc_mint);
    assert_err(env.send(&[ix], &[&admin]), PoolError::RoleCollision);
}

/// Writes the pool's stored layout version (first byte after the discriminator), as an upgrade
/// whose data is not yet migrated would leave it.
fn set_pool_version(env: &mut Env, version: u8) {
    let pool = env.pool();
    let mut a = env.svm.get_account(&pool).unwrap();
    a.data[8] = version;
    env.svm.set_account(pool, a).unwrap();
}

fn migrate(env: &mut Env) -> Result<(), String> {
    let caller = env.funded(0);
    let ix = env.ix(
        safu_pool::accounts::Migrate { pool: env.pool() },
        safu_pool::instruction::Migrate {},
    );
    env.send(&[ix], &[&caller]).map(|_| ())
}

/// After an upgrade the pool refuses every call until `migrate` brings its data to this version.
/// Anyone runs it, it is a no-op the second time, and a pool written by a newer version is refused.
#[test]
fn an_older_pool_refuses_every_call_until_migrate_runs() {
    let (mut env, roles) = funded_env();
    set_pool_version(&mut env, ACCOUNT_VERSION - 1);
    let s = env.funded(bounds().0);
    let ix = env.stake_ix(&s.pubkey(), bounds().0, s.pubkey());
    assert_err(
        env.send(std::slice::from_ref(&ix), &[&s]),
        PoolError::NotMigrated,
    );
    let pause = env.admin_ix(safu_pool::instruction::Pause {});
    assert_err(env.send(&[pause], &[&roles.admin]), PoolError::NotMigrated);
    migrate(&mut env).unwrap();
    assert_eq!(env.pool_state().version, ACCOUNT_VERSION);
    migrate(&mut env).unwrap();
    assert_eq!(env.pool_state().version, ACCOUNT_VERSION);
    env.send(&[ix], &[&s]).unwrap();
    set_pool_version(&mut env, ACCOUNT_VERSION + 1);
    assert_err(migrate(&mut env), PoolError::NotMigrated);
}
