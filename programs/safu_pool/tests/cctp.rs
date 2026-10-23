//! CCTP against Circle's real programs: devnet MessageTransmitterV2 and TokenMessengerMinterV2
//! (`fixtures/cctp_*.so`) with their state as dumped 2026-10-22, and the pool on Circle's devnet
//! USDC. The one change: Circle's two attester keys are swapped for two test keys, so the tests can
//! attest their own burns. Everything else (nonces, token pairs, minting, burning) is Circle's code.
//!
//! Owners sign as their wallets would: an Ethereum key with `personal_sign`, a Stellar key with
//! SEP-53 `signMessage` (here a Solana keypair: both are plain Ed25519).
mod common;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{InstructionData, ToAccountMetas};
use common::*;
use pool_core::cctp as c;
use safu_pool::constants::*;
use safu_pool::errors::PoolError;
use safu_pool::state::Pool;
use solana_keypair::Keypair;
use solana_signer::Signer;

use common::cctp::*;

// --- receive --------------------------------------------------------------------------------------

#[test]
fn an_ethereum_burn_lands_in_the_owners_account_once() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let amount = 250 * USDC;
    let message = t.burn(&owner, c::MODE_STAKE, amount);
    let ix = t.receive_ix(c::MODE_STAKE, &owner, message.clone());
    let r = t.relayer.insecure_clone();
    t.env.ok(std::slice::from_ref(&ix), &[&r]);
    let identity = t.identity(c::MODE_STAKE, &owner);
    let a = t.state(&identity);
    assert_eq!(
        (a.mode, a.home_domain, a.home_owner, a.to_deploy, a.nonce),
        (
            c::MODE_STAKE,
            c::domain::ETHEREUM,
            owner.bytes32(),
            amount,
            0
        )
    );
    assert_eq!(t.env.token_amount(&t.held(&identity)), amount);
    assert_eq!(t.env.token_amount(&t.inbox(c::MODE_STAKE)), 0);
    // Circle marks the nonce used: the same message never mints twice.
    t.env.svm.expire_blockhash();
    assert!(t.env.send(&[ix], &[&r]).is_err());
    assert_eq!(t.env.token_amount(&t.held(&identity)), amount);
}

#[test]
fn a_second_deposit_adds_to_the_same_account() {
    let mut t = Cctp::new();
    let owner = HomeKey::stellar();
    let identity = t.receive(c::MODE_BACK, &owner, 10 * USDC);
    t.receive(c::MODE_BACK, &owner, 5 * USDC);
    assert_eq!(t.state(&identity).to_deploy, 15 * USDC);
    assert_eq!(t.env.token_amount(&t.held(&identity)), 15 * USDC);
    // Each mode is its own account.
    assert_ne!(identity, t.identity(c::MODE_STAKE, &owner));
}

#[test]
fn receive_refuses_before_circle_so_the_message_stays_usable() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let r = t.relayer.insecure_clone();
    let (caller, stake_inbox) = (t.authority().to_bytes(), t.inbox(c::MODE_STAKE).to_bytes());

    // destinationCaller is someone else.
    let m = t.burn_to(&owner, c::domain::ETHEREUM, USDC, [9; 32], stake_inbox);
    let ix = t.receive_ix(c::MODE_STAKE, &owner, m);
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpWrongCaller);

    // Minted to the back inbox, received as a stake: the burn picked the mode.
    let m = t.burn(&owner, c::MODE_BACK, USDC);
    let ix = t.receive_ix(c::MODE_STAKE, &owner, m.clone());
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpWrongRecipient);
    // The same message, received as it was burned, still completes.
    let ix = t.receive_ix(c::MODE_BACK, &owner, m);
    t.env.ok(&[ix], &[&r]);

    // Claimed for another owner.
    let m = t.burn(&owner, c::MODE_STAKE, USDC);
    let ix = t.receive_ix(c::MODE_STAKE, &HomeKey::eth(), m);
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpWrongOwner);

    // A Solana burner is not a home chain here.
    let m = t.burn_to(&owner, c::domain::SOLANA, USDC, caller, stake_inbox);
    let valid = t.burn(&owner, c::MODE_STAKE, USDC);
    let mut ix = t.receive_ix(c::MODE_STAKE, &owner, valid);
    let args = safu_pool::instruction::CctpReceive {
        mode: c::MODE_STAKE,
        home_domain: c::domain::ETHEREUM,
        home_owner: owner.bytes32(),
        message: m.clone(),
        attestation: t.attest(&m),
    };
    ix.data = args.data();
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpUnsupportedDomain);
}

// --- deploy ---------------------------------------------------------------------------------------

#[test]
fn deploy_stakes_as_the_owner_with_payouts_to_their_own_account() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let amount = bounds().1;
    let identity = t.receive(c::MODE_STAKE, &owner, amount);
    t.deploy(&identity, c::MODE_STAKE).unwrap();
    let s = t.env.stake_state(&identity);
    assert_eq!(
        (s.staker, s.beneficiary, s.amount),
        (identity, identity, amount)
    );
    assert_eq!(t.state(&identity).to_deploy, 0);
    assert_eq!(t.env.token_amount(&t.held(&identity)), 0);
    assert_err(
        t.deploy(&identity, c::MODE_STAKE),
        PoolError::CctpNothingToDeploy,
    );
}

#[test]
fn deploy_backs_in_back_mode() {
    let mut t = Cctp::new();
    let owner = HomeKey::stellar();
    let identity = t.receive(c::MODE_BACK, &owner, 40 * USDC);
    t.deploy(&identity, c::MODE_BACK).unwrap();
    let b = t.env.backer_state(&identity);
    assert_eq!(b.backer, identity);
    assert_eq!(t.env.pool_state().total_backed + pending(&b), 40 * USDC);
}

fn pending(b: &safu_pool::state::BackerRecord) -> u64 {
    b.pending[..b.pending_count as usize]
        .iter()
        .map(|p| p.amount)
        .sum()
}

#[test]
fn deploy_must_use_the_owners_identity_and_held_account() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = t.receive(c::MODE_STAKE, &owner, bounds().1);
    let mut ix = t.deploy_ix(&identity, c::MODE_STAKE);
    // Someone else's USDC account in the staker's place.
    let at = t.call_accounts(&identity).len() + 5;
    ix.accounts[at].pubkey = t.env.ata(&t.relayer.pubkey());
    let r = t.relayer.insecure_clone();
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpCallNotAllowed);
}

#[test]
fn a_refused_deploy_leaves_the_usdc_held_and_it_can_go_home() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = t.receive(c::MODE_STAKE, &owner, bounds().1);
    let admin = t.env.admin.insecure_clone();
    let pause = t.env.admin_ix(safu_pool::instruction::Pause {});
    t.env.ok(&[pause], &[&admin]);
    assert_err(t.deploy(&identity, c::MODE_STAKE), PoolError::Paused);
    assert_eq!(t.state(&identity).to_deploy, bounds().1);
    assert_eq!(t.env.token_amount(&t.held(&identity)), bounds().1);
    t.send_home(&owner, &identity).unwrap();
    assert_eq!(t.env.token_amount(&t.held(&identity)), 0);
    assert_eq!(t.state(&identity).to_deploy, 0);
}

/// Sending home from another account the identity owns leaves the held USDC waiting to deploy.
#[test]
fn sending_home_from_another_account_keeps_the_held_usdc_waiting() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = t.receive(c::MODE_STAKE, &owner, bounds().1);
    t.env.open_usdc(&identity);
    let other = t.env.ata(&identity);
    t.env.add_token(&other, USDC);
    let event = Keypair::new();
    let nonce = t.state(&identity).nonce;
    let tx = t.send_home_from_tx(&owner, &identity, &event, nonce, other);
    let r = t.relayer.insecure_clone();
    t.env.send(&tx, &[&r, &event]).unwrap();
    assert_eq!(t.env.token_amount(&other), 0);
    assert_eq!(t.state(&identity).to_deploy, bounds().1);
    t.deploy(&identity, c::MODE_STAKE).unwrap();
}

// --- owner-signed calls ---------------------------------------------------------------------------

fn staked(t: &mut Cctp, owner: &HomeKey) -> Pubkey {
    let identity = t.receive(c::MODE_STAKE, owner, bounds().1);
    t.deploy(&identity, c::MODE_STAKE).unwrap();
    identity
}

#[test]
fn an_ethereum_owner_withdraws_into_their_own_account() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = staked(&mut t, &owner);
    let amount = bounds().1;
    let inner = t.withdraw_metas(&identity);
    let data = safu_pool::instruction::Withdraw { amount }.data();
    t.act(&owner, &identity, inner, data).unwrap();
    assert_eq!(t.env.token_amount(&t.held(&identity)), amount);
    assert_eq!(t.state(&identity).nonce, 1);
    assert_eq!(t.env.stake_state(&identity).amount, 0);
}

#[test]
fn a_stellar_owner_signs_with_sep53() {
    let mut t = Cctp::new();
    let owner = HomeKey::stellar();
    let identity = staked(&mut t, &owner);
    let inner = t.withdraw_metas(&identity);
    let data = safu_pool::instruction::Withdraw { amount: bounds().1 }.data();
    t.act(&owner, &identity, inner, data).unwrap();
    assert_eq!(t.env.token_amount(&t.held(&identity)), bounds().1);
}

#[test]
fn a_signature_works_once_for_its_owner_its_call_and_its_time() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = staked(&mut t, &owner);
    let r = t.relayer.insecure_clone();
    let half = bounds().1 / 2;
    let data = safu_pool::instruction::Withdraw { amount: half }.data();
    let inner = t.withdraw_metas(&identity);
    let deadline = t.env.now + 60;

    // Someone else's key.
    let tx = t.act_tx(
        &HomeKey::eth(),
        &identity,
        inner.clone(),
        data.clone(),
        0,
        deadline,
    );
    assert_err(t.env.send(&tx, &[&r]), PoolError::CctpBadSignature);
    // A Stellar signature for an Ethereum owner.
    let tx = t.act_tx(
        &HomeKey::stellar(),
        &identity,
        inner.clone(),
        data.clone(),
        0,
        deadline,
    );
    assert_err(t.env.send(&tx, &[&r]), PoolError::CctpWrongSignatureKind);
    // Signed for one amount, sent with another.
    let mut tx = t.act_tx(&owner, &identity, inner.clone(), data.clone(), 0, deadline);
    let other = safu_pool::instruction::Withdraw { amount: half - 1 }.data();
    let mut args = tx[0].data[8..].to_vec();
    let at = args.len() - data.len();
    args.splice(at.., other);
    tx[0].data.truncate(8);
    tx[0].data.extend(args);
    assert_err(t.env.send(&tx, &[&r]), PoolError::CctpBadSignature);
    // Too far out, then expired.
    let tx = t.act_tx(
        &owner,
        &identity,
        inner.clone(),
        data.clone(),
        0,
        t.env.now + MAX_ACTION_TTL_SECS + 1,
    );
    assert_err(t.env.send(&tx, &[&r]), PoolError::SignatureDeadlineTooFar);
    let tx = t.act_tx(&owner, &identity, inner.clone(), data.clone(), 0, deadline);
    t.env.warp(61);
    assert_err(t.env.send(&tx, &[&r]), PoolError::SignatureExpired);

    // A good one works once.
    let deadline = t.env.now + 60;
    let tx = t.act_tx(&owner, &identity, inner.clone(), data.clone(), 0, deadline);
    t.env.ok(&tx, &[&r]);
    t.env.svm.expire_blockhash();
    assert_err(t.env.send(&tx, &[&r]), PoolError::CctpBadNonce);
}

#[test]
fn an_owner_cannot_send_payouts_anywhere_else() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = staked(&mut t, &owner);
    let thief = t.env.funded(0).pubkey();

    // No beneficiary change at all.
    let set = safu_pool::accounts::SetBeneficiary {
        staker: identity,
        pool: t.pool(),
        stake_record: t.env.stake_record(&identity),
    };
    let inner = Cctp::as_identity(set.to_account_metas(None), &identity);
    let data = safu_pool::instruction::SetBeneficiary { beneficiary: thief }.data();
    assert_err(
        t.act(&owner, &identity, inner, data),
        PoolError::CctpCallNotAllowed,
    );

    // A withdrawal into someone else's USDC account: the pool pays only the beneficiary.
    t.env.open_usdc(&thief);
    let mut inner = t.withdraw_metas(&identity);
    inner[4].pubkey = t.env.ata(&thief);
    let data = safu_pool::instruction::Withdraw { amount: bounds().1 }.data();
    assert_err(
        t.act(&owner, &identity, inner, data),
        PoolError::WrongBeneficiary,
    );

    // A new stake naming someone else as beneficiary.
    let held = t.held(&identity);
    t.env.add_token(&held, USDC);
    let inner = t.stake_metas(&identity);
    let data = safu_pool::instruction::Stake {
        amount: USDC,
        beneficiary: thief,
    }
    .data();
    assert_err(
        t.act(&owner, &identity, inner, data),
        PoolError::CctpBeneficiaryNotOwn,
    );
}

/// The signature fixes where the call's data ends and its accounts begin: the same signature, with
/// the signed accounts' bytes moved into the data and no signed accounts, is refused, so the accounts
/// it would then run with (here a payout account the owner never signed) cannot be swapped in.
#[test]
fn a_signature_cannot_be_resplit_into_other_data_and_accounts() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = staked(&mut t, &owner);
    let r = t.relayer.insecure_clone();
    let amount = bounds().1;
    let inner = t.withdraw_metas(&identity);
    let data = safu_pool::instruction::Withdraw { amount }.data();
    let nonce = t.state(&identity).nonce;
    let expires_at = t.env.now + 60;
    let mut view = t.call_accounts(&identity);
    view.extend(inner.iter().cloned());
    let view = [Instruction {
        program_id: safu_pool::ID,
        accounts: view,
        data: vec![],
    }];
    let signed = seen(&view, &r.pubkey(), &inner);
    let (signature, _) = t.sign(
        &owner,
        &identity,
        nonce,
        expires_at,
        &call_hash(&data, &signed),
    );

    let mut moved = data.clone();
    for m in &signed {
        moved.extend_from_slice(&m.pubkey.to_bytes());
        moved.extend([u8::from(m.is_writable), u8::from(m.is_signer)]);
    }
    t.env.open_usdc(&identity);
    let mut swapped = inner.clone();
    swapped[4].pubkey = t.env.ata(&identity);
    for accounts in [inner, swapped] {
        let mut metas = t.call_accounts(&identity);
        metas.extend(accounts);
        let ix = Instruction {
            program_id: safu_pool::ID,
            accounts: metas,
            data: safu_pool::instruction::CctpAct {
                nonce,
                expires_at,
                signature: signature.clone(),
                signed_accounts: 0,
                data: moved.clone(),
            }
            .data(),
        };
        assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpBadSignature);
    }
    assert_eq!(t.env.token_amount(&t.held(&identity)), 0);
    assert_eq!(t.env.stake_state(&identity).amount, amount);
}

/// Same vector as the relayer's test (backend `test_solana_relay.py`): both build the same digest.
#[test]
fn the_call_hash_matches_the_relayers() {
    let metas = [
        AccountMeta::new(Pubkey::new_from_array([7; 32]), false),
        AccountMeta::new_readonly(Pubkey::new_from_array([9; 32]), true),
    ];
    let hex: String = call_hash(&[1, 2], &metas)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex,
        "bc8240d8209553bf09e4f94e09ced89168d82d13b9a1a6ebed1a6d930c0ca75d"
    );
}

// --- send home ------------------------------------------------------------------------------------

#[test]
fn ethereum_owners_get_their_usdc_back_on_ethereum() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = t.receive(c::MODE_STAKE, &owner, 12 * USDC);
    let m = t.send_home(&owner, &identity).unwrap();
    let body = c::header::BODY;
    assert_eq!(&m[8..12], &c::domain::ETHEREUM.to_be_bytes());
    assert_eq!(read_32(&m, c::header::DESTINATION_CALLER), [0u8; 32]);
    assert_eq!(read_32(&m, body + c::burn::MINT_RECIPIENT), owner.bytes32());
    assert_eq!(
        read_32(&m, body + c::burn::MESSAGE_SENDER),
        identity.to_bytes()
    );
    assert_eq!(
        &m[body + c::burn::AMOUNT + 24..body + c::burn::AMOUNT + 32],
        &(12 * USDC).to_be_bytes()
    );
    assert_eq!(m.len(), body + c::burn::HOOK_DATA);
    assert_eq!(t.env.token_amount(&t.held(&identity)), 0);
    assert_err(
        t.send_home(&owner, &identity).map(|_| ()),
        PoolError::CctpNothingToSend,
    );
}

#[test]
fn stellar_owners_get_theirs_through_circles_forwarder() {
    let mut t = Cctp::new();
    let owner = HomeKey::stellar();
    let identity = t.receive(c::MODE_BACK, &owner, 7 * USDC);
    let m = t.send_home(&owner, &identity).unwrap();
    let forwarder = t.env.pool_state().stellar_forwarder;
    let body = c::header::BODY;
    assert_eq!(&m[8..12], &c::domain::STELLAR.to_be_bytes());
    assert_eq!(read_32(&m, c::header::DESTINATION_CALLER), forwarder);
    assert_eq!(read_32(&m, body + c::burn::MINT_RECIPIENT), forwarder);
    let owner_key: [u8; 32] = owner.bytes32();
    assert_eq!(
        &m[body + c::burn::HOOK_DATA..],
        &c::forward_hook(&owner_key)
    );
    assert_eq!(t.env.token_amount(&t.held(&identity)), 0);
}

#[test]
fn only_the_owner_can_send_home() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = t.receive(c::MODE_STAKE, &owner, 3 * USDC);
    let event = Keypair::new();
    let tx = t.send_home_tx(&HomeKey::eth(), &identity, &event, 0);
    let r = t.relayer.insecure_clone();
    assert_err(t.env.send(&tx, &[&r, &event]), PoolError::CctpBadSignature);
    assert_eq!(t.env.token_amount(&t.held(&identity)), 3 * USDC);
}

#[test]
fn the_pool_keeps_its_stellar_forwarder_from_config() {
    let t = Cctp::new();
    let p: Pool = t.env.pool_state();
    assert_eq!(p.stellar_forwarder, config().stellar_forwarder);
    assert_ne!(p.stellar_forwarder, [0u8; 32]);
}

/// The owner-signed path costs little on top of the call it runs: the busiest user call measures
/// 260-263k (`kamino.rs`, harvest + push + upkeep), so with this extra it still fits the site's
/// 400k limit. Receive (about 240k, mostly Circle checking two attester signatures and minting),
/// deploy and send home fit on their own.
#[test]
fn owner_calls_and_bridging_fit_the_site_limit() {
    let mut t = Cctp::new();
    let limit = t.env.cfg.compute_unit_limit;
    let r = t.relayer.insecure_clone();
    for owner in [HomeKey::eth(), HomeKey::stellar()] {
        let m = t.burn(&owner, c::MODE_STAKE, bounds().1);
        let ix = t.receive_ix(c::MODE_STAKE, &owner, m);
        let receive = t.env.send_cu(&[ix], &[&r]).unwrap();
        let identity = t.identity(c::MODE_STAKE, &owner);
        let ix = t.deploy_ix(&identity, c::MODE_STAKE);
        let deploy = t.env.send_cu(&[ix], &[&r]).unwrap();

        let nonce = t.state(&identity).nonce;
        let inner = t.withdraw_metas(&identity);
        let data = safu_pool::instruction::Withdraw { amount: bounds().1 }.data();
        let tx = t.act_tx(&owner, &identity, inner, data, nonce, t.env.now + 60);
        let act = t.env.send_cu(&tx, &[&r]).unwrap();
        let plain = t.env.staker(bounds().1);
        let ix = t.env.withdraw_ix(&plain.pubkey(), &plain.pubkey());
        let base = t.env.send_cu(&[ix], &[&plain]).unwrap();

        let event = Keypair::new();
        let tx = t.send_home_tx(&owner, &identity, &event, nonce + 1);
        let home = t.env.send_cu(&tx, &[&r, &event]).unwrap();
        println!(
            "domain {}: receive {receive}, deploy {deploy}, act {act} (+{}), send home {home}",
            owner.domain(),
            act - base
        );
        assert!(act - base <= 60_000, "owner-signed overhead {}", act - base);
        for cu in [receive, deploy, act, home] {
            assert!(cu <= limit, "{cu} of {limit}");
        }
    }
}

#[test]
fn a_signature_made_for_another_cluster_does_not_work_here() {
    let mut t = Cctp::new();
    let owner = HomeKey::eth();
    let identity = staked(&mut t, &owner);
    let cluster = t.env.pool_state().cluster;
    t.sign_cluster = Some(cluster + 1);
    let inner = t.withdraw_metas(&identity);
    let data = safu_pool::instruction::Withdraw { amount: bounds().1 }.data();
    assert_err(
        t.act(&owner, &identity, inner.clone(), data.clone()),
        PoolError::CctpBadSignature,
    );
    t.sign_cluster = None;
    t.act(&owner, &identity, inner, data).unwrap();
}

#[test]
fn an_inbox_opens_only_for_stake_or_back() {
    let mut t = Cctp::new();
    let bad = 7;
    assert!(bad != c::MODE_STAKE && bad != c::MODE_BACK);
    let ix = t.open_inbox_ix(bad);
    let r = t.relayer.insecure_clone();
    assert_err(t.env.send(&[ix], &[&r]), PoolError::CctpWrongRecipient);
}
