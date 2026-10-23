//! CCTP v2: stake and back from Ethereum and Stellar (multichain `cctp-adapter` + `safu-account`,
//! one program here).
//!
//! 1. The owner burns USDC at home naming this pool's inbox for the mode as `mintRecipient` and its
//!    CCTP authority as `destinationCaller`, so only this program can complete the message.
//! 2. `cctp_receive` (anyone; SAFU's relayer, which pays rent): checks both, calls Circle's
//!    `receive_message`, measures the mint by balance, moves it to the owner's `held` account and
//!    marks it `to_deploy`. It never refuses for pool reasons, so a mint can always complete.
//! 3. `cctp_deploy` (anyone): stakes or backs `to_deploy` as the owner's identity, as their burn
//!    asked. If the pool refuses (full, paused, already staked), that call fails and the USDC waits
//!    in `held`, never lost.
//! 4. `cctp_act` (owner-signed): runs one allowed pool call as the owner's identity.
//! 5. `cctp_send_home` (owner-signed): burns the account's USDC home. Ethereum: to the owner's
//!    address. Stellar: through Circle's CctpForwarder to the owner's `G...` wallet.
//!
//! Money leaves an owner's account only into the pool or home to the owner: no call takes a
//! destination, and a stake's beneficiary must be the identity.
//!
//! Owner signatures (`pool_core::cctp::action_preimage`): the owner signs a 32-byte digest.
//! - Ethereum: `personal_sign` of the 32 bytes, checked by secp256k1 recovery.
//! - Stellar: SEP-53 `signMessage` of the digest as 64 lowercase hex characters, checked by the
//!   Ed25519 precompile instruction placed right before this one (wallets sign text).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::solana_program::program::invoke_signed;
use anchor_lang::{Discriminator, InstructionData};
use anchor_spl::token::{self, Mint, Token, TokenAccount, TransferChecked};
use pool_core::cctp::{self as c, Owner as HomeOwner};

use crate::approval::verify_preceding_ed25519;
use crate::constants::*;
use crate::errors::{CoreResultExt, PoolError};
use crate::events::{CctpActed, CctpDeployed, CctpReceived, CctpSentHome};
use crate::instruction as ix;
use crate::state::{CctpAccount, Pool, ACCOUNT_VERSION};

/// How the owner signed. A Stellar signature travels in the Ed25519 precompile instruction.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub enum OwnerSignature {
    /// r || s || v (v 27/28 or 0/1).
    Evm([u8; 65]),
    Stellar,
}

/// Pool calls a CCTP owner may make. `set_beneficiary` is not one: payouts stay with the identity.
pub(crate) const ALLOWED_CALLS: [&[u8]; 11] = [
    ix::Stake::DISCRIMINATOR,
    ix::Back::DISCRIMINATOR,
    ix::Withdraw::DISCRIMINATOR,
    ix::EmergencyExit::DISCRIMINATOR,
    ix::ClaimYield::DISCRIMINATOR,
    ix::ApproveClaim::DISCRIMINATOR,
    ix::ClaimStream::DISCRIMINATOR,
    ix::RequestBackerWithdrawal::DISCRIMINATOR,
    ix::CancelBackerWithdrawal::DISCRIMINATOR,
    ix::CompleteBackerWithdrawal::DISCRIMINATOR,
    ix::ClaimBackerYield::DISCRIMINATOR,
];

/// In `stake` and `back`, the staker/backer is account 1 and its USDC account 5 (after the payer).
const DEPOSITOR_AT: usize = 1;
const DEPOSITOR_USDC_AT: usize = 5;

fn identity_seeds<'a>(
    pool: &'a Pubkey,
    mode: &'a [u8; 1],
    domain: &'a [u8; 4],
    owner: &'a [u8; 32],
    bump: &'a [u8; 1],
) -> [&'a [u8]; 6] {
    [SEED_CCTP_IDENTITY, pool.as_ref(), mode, domain, owner, bump]
}

/// Calls `program_id` with `accounts` in order, each with whether it is writable. Signers are this
/// transaction's own plus `pda`, which this program signs as with `seeds`.
fn invoke_as(
    program_id: Pubkey,
    accounts: &[(AccountInfo<'_>, bool)],
    pda: &Pubkey,
    data: Vec<u8>,
    seeds: &[&[u8]],
) -> Result<()> {
    let metas = accounts
        .iter()
        .map(|(a, writable)| AccountMeta {
            pubkey: a.key(),
            is_signer: a.is_signer || a.key() == *pda,
            is_writable: *writable,
        })
        .collect();
    let infos: Vec<AccountInfo> = accounts.iter().map(|(a, _)| a.clone()).collect();
    let call = Instruction {
        program_id,
        accounts: metas,
        data,
    };
    invoke_signed(&call, &infos, &[seeds])?;
    Ok(())
}

/// Runs `data` on this program with `accounts`, the identity signing.
fn call_as_identity<'info>(
    program: &AccountInfo<'info>,
    account: &CctpAccount,
    pool: &Pubkey,
    identity: &Pubkey,
    accounts: &[AccountInfo<'info>],
    data: Vec<u8>,
) -> Result<()> {
    let metas = accounts
        .iter()
        .map(|a| AccountMeta {
            pubkey: a.key(),
            is_signer: a.is_signer || a.key() == *identity,
            is_writable: a.is_writable,
        })
        .collect();
    let call = Instruction {
        program_id: crate::ID,
        accounts: metas,
        data,
    };
    let mut infos = accounts.to_vec();
    infos.push(program.clone());
    let (mode, domain, bump) = (
        [account.mode],
        account.home_domain.to_be_bytes(),
        [account.identity_bump],
    );
    let seeds = identity_seeds(pool, &mode, &domain, &account.home_owner, &bump);
    invoke_signed(&call, &infos, &[&seeds])?;
    Ok(())
}

/// Checks the owner's signature over this action and uses up its nonce.
fn authorize(
    account: &mut CctpAccount,
    account_key: &Pubkey,
    instructions: &AccountInfo,
    nonce: u64,
    expires_at: i64,
    signature: &OwnerSignature,
    call_hash: &[u8; 32],
) -> Result<()> {
    c::check_action_expiry(super::now()?, expires_at).core()?;
    require!(nonce == account.nonce, PoolError::CctpBadNonce);
    let digest = solana_sha256_hasher::hash(&c::action_preimage(
        &crate::ID.to_bytes(),
        account.cluster,
        &account_key.to_bytes(),
        nonce,
        expires_at,
        call_hash,
    ))
    .to_bytes();
    match (account.home_domain, signature) {
        (c::domain::ETHEREUM, OwnerSignature::Evm(sig)) => {
            let message = solana_keccak_hasher::hashv(&[c::EIP191_PREFIX_32, &digest]).to_bytes();
            let v = sig[64];
            let recovery = v.checked_sub(c::EVM_V_OFFSET).unwrap_or(v);
            require!(recovery <= 1, PoolError::CctpBadSignature);
            let key = solana_secp256k1_recover::secp256k1_recover(&message, recovery, &sig[..64])
                .map_err(|_| PoolError::CctpBadSignature)?;
            let address = solana_keccak_hasher::hash(&key.to_bytes()).to_bytes();
            require!(
                address[c::EVM_PADDING..] == account.home_owner[c::EVM_PADDING..],
                PoolError::CctpBadSignature
            );
        }
        (c::domain::STELLAR, OwnerSignature::Stellar) => {
            let text = c::hex_lower(&digest);
            let message = solana_sha256_hasher::hashv(&[c::SEP53_PREFIX, &text]).to_bytes();
            // Any failure (no precompile, other key, other message) is the same answer here.
            verify_preceding_ed25519(
                instructions,
                &Pubkey::new_from_array(account.home_owner),
                &message,
            )
            .map_err(|_| error!(PoolError::CctpBadSignature))?;
        }
        _ => return err!(PoolError::CctpWrongSignatureKind),
    }
    account.nonce = account
        .nonce
        .checked_add(1)
        .ok_or(PoolError::MathOverflow)?;
    Ok(())
}

/// What the owner signs for a pool call: the data's length (u32 LE) and bytes, the number of
/// accounts (u8), then each account's key, writable and signer flags, in order. The lengths fix where
/// the data ends and the accounts begin, so a signature cannot be re-split into other data and other
/// accounts.
pub fn call_hash(data: &[u8], accounts: &[AccountInfo]) -> Result<[u8; 32]> {
    let data_len = u32::try_from(data.len()).map_err(|_| PoolError::CctpCallNotAllowed)?;
    let count = u8::try_from(accounts.len()).map_err(|_| PoolError::CctpCallNotAllowed)?;
    let mut parts: Vec<Vec<u8>> = Vec::with_capacity(accounts.len() + 3);
    parts.push(data_len.to_le_bytes().to_vec());
    parts.push(data.to_vec());
    parts.push(vec![count]);
    for a in accounts {
        let mut p = a.key().to_bytes().to_vec();
        p.push(u8::from(a.is_writable));
        p.push(u8::from(a.is_signer));
        parts.push(p);
    }
    let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
    Ok(solana_sha256_hasher::hashv(&refs).to_bytes())
}

// --- inbox ----------------------------------------------------------------------------------------

#[derive(Accounts)]
#[instruction(mode: u8)]
pub struct CctpOpenInbox<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: PDA with no data; signs Circle's receive as the burn's `destinationCaller`.
    #[account(seeds = [SEED_CCTP_AUTHORITY, pool.key().as_ref()], bump)]
    pub authority: UncheckedAccount<'info>,
    #[account(
        init,
        payer = payer,
        seeds = [SEED_CCTP_INBOX, pool.key().as_ref(), &[mode]],
        bump,
        token::mint = usdc_mint,
        token::authority = authority,
        token::token_program = token_program,
    )]
    pub inbox: Box<Account<'info, TokenAccount>>,
    #[account(address = pool.usdc_mint @ PoolError::WrongMint)]
    pub usdc_mint: Box<Account<'info, Mint>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// Opens the inbox Circle mints a mode's deposits into. Anyone, once per mode.
pub fn cctp_open_inbox(_ctx: Context<CctpOpenInbox>, mode: u8) -> Result<()> {
    require!(
        mode == c::MODE_STAKE || mode == c::MODE_BACK,
        PoolError::CctpWrongRecipient
    );
    Ok(())
}

// --- receive --------------------------------------------------------------------------------------

/// Circle's accounts for `receive_message`, in the order its handler takes them. Circle checks every
/// one; the pool checks only that the programs are Circle's and that the mint lands in its inbox.
#[derive(Accounts)]
pub struct CircleReceive<'info> {
    /// CHECK: Circle MessageTransmitterV2.
    #[account(address = CCTP_MESSAGE_TRANSMITTER_ID)]
    pub message_transmitter_program: UncheckedAccount<'info>,
    /// CHECK: Circle TokenMessengerMinterV2.
    #[account(address = CCTP_TOKEN_MESSENGER_MINTER_ID)]
    pub token_messenger_minter_program: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub message_transmitter_authority: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub message_transmitter: UncheckedAccount<'info>,
    /// CHECK: checked (and created) by Circle; marks the message used.
    #[account(mut)]
    pub used_nonce: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub message_transmitter_event_authority: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub token_messenger: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub remote_token_messenger: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    #[account(mut)]
    pub token_minter: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    #[account(mut)]
    pub local_token: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub token_pair: UncheckedAccount<'info>,
    /// CHECK: checked by Circle (its fee recipient's USDC account).
    #[account(mut)]
    pub fee_recipient_token_account: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    #[account(mut)]
    pub custody: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub token_messenger_event_authority: UncheckedAccount<'info>,
}

#[derive(Accounts)]
#[instruction(mode: u8, home_domain: u32, home_owner: [u8; 32])]
pub struct CctpReceive<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: PDA with no data (see `CctpOpenInbox::authority`).
    #[account(seeds = [SEED_CCTP_AUTHORITY, pool.key().as_ref()], bump)]
    pub authority: UncheckedAccount<'info>,
    #[account(mut, seeds = [SEED_CCTP_INBOX, pool.key().as_ref(), &[mode]], bump)]
    pub inbox: Box<Account<'info, TokenAccount>>,
    /// CHECK: PDA with no data: the owner's staker or backer key, signed for only by this program.
    #[account(
        seeds = [SEED_CCTP_IDENTITY, pool.key().as_ref(), &[mode], &home_domain.to_be_bytes(), &home_owner],
        bump,
    )]
    pub identity: UncheckedAccount<'info>,
    #[account(
        init_if_needed,
        payer = payer,
        space = 8 + CctpAccount::INIT_SPACE,
        seeds = [SEED_CCTP_ACCOUNT, identity.key().as_ref()],
        bump,
    )]
    pub account: Box<Account<'info, CctpAccount>>,
    #[account(
        init_if_needed,
        payer = payer,
        seeds = [SEED_CCTP_HELD, identity.key().as_ref()],
        bump,
        token::mint = usdc_mint,
        token::authority = identity,
        token::token_program = token_program,
    )]
    pub held: Box<Account<'info, TokenAccount>>,
    #[account(address = pool.usdc_mint @ PoolError::WrongMint)]
    pub usdc_mint: Box<Account<'info, Mint>>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub circle: CircleReceive<'info>,
}

/// Completes a home-chain burn into the owner's account. Every check runs BEFORE Circle's receive:
/// a refused call leaves the message unused, so it can still complete once the problem is fixed.
/// Never checks pause or pool limits (those apply at `cctp_deploy`).
pub fn cctp_receive(
    mut ctx: Context<CctpReceive>,
    mode: u8,
    home_domain: u32,
    home_owner: [u8; 32],
    message: Vec<u8>,
    attestation: Vec<u8>,
) -> Result<()> {
    let burn = c::parse_inbound(&message).ok_or(PoolError::CctpMalformedMessage)?;
    let a = &ctx.accounts;
    require!(
        burn.destination_caller == a.authority.key().to_bytes(),
        PoolError::CctpWrongCaller
    );
    require!(
        burn.mint_recipient == a.inbox.key().to_bytes(),
        PoolError::CctpWrongRecipient
    );
    require!(
        burn.destination_domain == c::domain::SOLANA && burn.source_domain == home_domain,
        PoolError::CctpUnsupportedDomain
    );
    let burner = HomeOwner::from_sender(burn.source_domain, &burn.message_sender)
        .ok_or(PoolError::CctpUnsupportedDomain)?;
    require!(burner.to_bytes32() == home_owner, PoolError::CctpWrongOwner);
    c::bridge_scale(a.usdc_mint.decimals, c::CANONICAL_DECIMALS).core()?;

    let pool_key = a.pool.key();
    let authority_bump = [ctx.bumps.authority];
    let authority_seeds: &[&[u8]] = &[SEED_CCTP_AUTHORITY, pool_key.as_ref(), &authority_bump];
    let before = a.inbox.amount;
    let circle = &a.circle;
    let mut data = c::DISC_RECEIVE_MESSAGE.to_vec();
    (message, attestation).serialize(&mut data)?;
    invoke_as(
        CCTP_MESSAGE_TRANSMITTER_ID,
        &[
            (a.payer.to_account_info(), true),
            (a.authority.to_account_info(), false),
            (
                circle.message_transmitter_authority.to_account_info(),
                false,
            ),
            (circle.message_transmitter.to_account_info(), false),
            (circle.used_nonce.to_account_info(), true),
            (
                circle.token_messenger_minter_program.to_account_info(),
                false,
            ),
            (a.system_program.to_account_info(), false),
            (
                circle.message_transmitter_event_authority.to_account_info(),
                false,
            ),
            (circle.message_transmitter_program.to_account_info(), false),
            // Remaining accounts: the token messenger's handler.
            (circle.token_messenger.to_account_info(), false),
            (circle.remote_token_messenger.to_account_info(), false),
            (circle.token_minter.to_account_info(), true),
            (circle.local_token.to_account_info(), true),
            (circle.token_pair.to_account_info(), false),
            (circle.fee_recipient_token_account.to_account_info(), true),
            (a.inbox.to_account_info(), true),
            (circle.custody.to_account_info(), true),
            (a.token_program.to_account_info(), false),
            (
                circle.token_messenger_event_authority.to_account_info(),
                false,
            ),
            (
                circle.token_messenger_minter_program.to_account_info(),
                false,
            ),
        ],
        &a.authority.key(),
        data,
        authority_seeds,
    )?;

    let a = &mut ctx.accounts;
    a.inbox.reload()?;
    let minted = a.inbox.amount.saturating_sub(before);
    require!(minted > 0, PoolError::CctpNothingMinted);
    token::transfer_checked(
        CpiContext::new_with_signer(
            a.token_program.key(),
            TransferChecked {
                from: a.inbox.to_account_info(),
                mint: a.usdc_mint.to_account_info(),
                to: a.held.to_account_info(),
                authority: a.authority.to_account_info(),
            },
            &[authority_seeds],
        ),
        minted,
        a.usdc_mint.decimals,
    )?;

    let account = &mut a.account;
    if account.version == 0 {
        account.version = ACCOUNT_VERSION;
        account.mode = mode;
        account.home_domain = home_domain;
        account.cluster = a.pool.cluster;
        account.home_owner = home_owner;
        account.bump = ctx.bumps.account;
        account.identity_bump = ctx.bumps.identity;
    }
    account.to_deploy = pool_core::add(account.to_deploy, minted).core()?;
    emit!(CctpReceived {
        account: account.key(),
        identity: a.identity.key(),
        home_domain,
        mode,
        amount: minted,
    });
    Ok(())
}

// --- deploy and act -------------------------------------------------------------------------------

/// The owner's account and identity. The pool call's own accounts follow as remaining accounts, in
/// that call's order; the call checks them itself.
#[derive(Accounts)]
pub struct CctpCall<'info> {
    #[account(
        mut,
        seeds = [SEED_CCTP_ACCOUNT, identity.key().as_ref()],
        bump = account.bump,
    )]
    pub account: Box<Account<'info, CctpAccount>>,
    /// CHECK: the account's identity PDA (seeds from the account).
    #[account(
        seeds = [
            SEED_CCTP_IDENTITY,
            pool.key().as_ref(),
            &[account.mode],
            &account.home_domain.to_be_bytes(),
            &account.home_owner,
        ],
        bump = account.identity_bump,
    )]
    pub identity: UncheckedAccount<'info>,
    /// CHECK: the pool PDA. Not loaded here: the pool call loads and writes it.
    #[account(seeds = [SEED_POOL], bump)]
    pub pool: UncheckedAccount<'info>,
    /// CHECK: the instructions sysvar (a Stellar owner's Ed25519 check).
    #[account(address = solana_sdk_ids::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub program: Program<'info, crate::program::SafuPool>,
}

/// Stakes or backs what arrived by CCTP, as the owner's burn asked. Anyone may call it. Remaining
/// accounts: `stake`'s (staker = identity, USDC = held) or `back`'s, payer first.
pub fn cctp_deploy<'info>(ctx: Context<'info, CctpCall<'info>>) -> Result<()> {
    let accounts = ctx.remaining_accounts;
    let identity = ctx.accounts.identity.key();
    let held = Pubkey::find_program_address(&[SEED_CCTP_HELD, identity.as_ref()], &crate::ID).0;
    require!(
        accounts.get(DEPOSITOR_AT).map(|a| a.key()) == Some(identity)
            && accounts.get(DEPOSITOR_USDC_AT).map(|a| a.key()) == Some(held),
        PoolError::CctpCallNotAllowed
    );
    let balance = crate::vault::token_amount(&accounts[DEPOSITOR_USDC_AT])?;
    let account = &mut ctx.accounts.account;
    let amount = account.to_deploy.min(balance);
    require!(amount > 0, PoolError::CctpNothingToDeploy);
    account.to_deploy -= amount;
    let data = if account.mode == c::MODE_STAKE {
        ix::Stake {
            amount,
            beneficiary: identity,
        }
        .data()
    } else {
        ix::Back { amount }.data()
    };
    call_as_identity(
        &ctx.accounts.program.to_account_info(),
        account,
        &ctx.accounts.pool.key(),
        &identity,
        accounts,
        data,
    )?;
    emit!(CctpDeployed {
        identity,
        mode: account.mode,
        amount
    });
    Ok(())
}

/// Runs one allowed pool call (`data`, accounts as remaining accounts) as the owner's identity,
/// once the owner's signature over exactly that call checks out. The owner signs the call's data and
/// its first `signed_accounts` accounts (the call's own); the upkeep accounts after them change as
/// the pool moves, are checked by the pool, and move no money, so a moved pool needs no new
/// signature.
pub fn cctp_act<'info>(
    ctx: Context<'info, CctpCall<'info>>,
    nonce: u64,
    expires_at: i64,
    signature: OwnerSignature,
    signed_accounts: u8,
    data: Vec<u8>,
) -> Result<()> {
    let identity = ctx.accounts.identity.key();
    let call = data.get(..8).ok_or(PoolError::CctpCallNotAllowed)?;
    require!(ALLOWED_CALLS.contains(&call), PoolError::CctpCallNotAllowed);
    if call == ix::Stake::DISCRIMINATOR {
        let args = ix::Stake::try_from_slice(&data[8..])?;
        require_keys_eq!(args.beneficiary, identity, PoolError::CctpBeneficiaryNotOwn);
    }
    let accounts = ctx.remaining_accounts;
    let signed = accounts
        .get(..usize::from(signed_accounts))
        .ok_or(PoolError::CctpCallNotAllowed)?;
    let hash = call_hash(&data, signed)?;
    let account_key = ctx.accounts.account.key();
    let account = &mut ctx.accounts.account;
    authorize(
        account,
        &account_key,
        &ctx.accounts.instructions,
        nonce,
        expires_at,
        &signature,
        &hash,
    )?;
    let mut tag = [0u8; 8];
    tag.copy_from_slice(call);
    call_as_identity(
        &ctx.accounts.program.to_account_info(),
        account,
        &ctx.accounts.pool.key(),
        &identity,
        accounts,
        data,
    )?;
    emit!(CctpActed {
        identity,
        nonce,
        call: tag
    });
    Ok(())
}

// --- send home ------------------------------------------------------------------------------------

/// Circle's accounts for `deposit_for_burn`, in the order its handler takes them.
#[derive(Accounts)]
pub struct CircleBurn<'info> {
    /// CHECK: Circle MessageTransmitterV2.
    #[account(address = CCTP_MESSAGE_TRANSMITTER_ID)]
    pub message_transmitter_program: UncheckedAccount<'info>,
    /// CHECK: Circle TokenMessengerMinterV2.
    #[account(address = CCTP_TOKEN_MESSENGER_MINTER_ID)]
    pub token_messenger_minter_program: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub sender_authority: UncheckedAccount<'info>,
    /// CHECK: checked by Circle (the burner is not denylisted).
    pub denylist_account: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    #[account(mut)]
    pub message_transmitter: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub token_messenger: UncheckedAccount<'info>,
    /// CHECK: checked by Circle (the home chain's messenger).
    pub remote_token_messenger: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub token_minter: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    #[account(mut)]
    pub local_token: UncheckedAccount<'info>,
    /// CHECK: checked by Circle.
    pub token_messenger_event_authority: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CctpSendHome<'info> {
    /// Pays rent for Circle's message account (reclaimable from Circle later).
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [SEED_POOL], bump = pool.bump, constraint = pool.is_current() @ PoolError::NotMigrated)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(
        mut,
        seeds = [SEED_CCTP_ACCOUNT, identity.key().as_ref()],
        bump = account.bump,
    )]
    pub account: Box<Account<'info, CctpAccount>>,
    /// CHECK: the account's identity PDA (seeds from the account).
    #[account(
        seeds = [
            SEED_CCTP_IDENTITY,
            pool.key().as_ref(),
            &[account.mode],
            &account.home_domain.to_be_bytes(),
            &account.home_owner,
        ],
        bump = account.identity_bump,
    )]
    pub identity: UncheckedAccount<'info>,
    /// Any USDC account the identity owns (`held`, or one a payout landed in).
    #[account(
        mut,
        constraint = from.owner == identity.key() @ PoolError::WrongTokenAccount,
        constraint = from.mint == pool.usdc_mint @ PoolError::WrongMint,
    )]
    pub from: Box<Account<'info, TokenAccount>>,
    #[account(mut, address = pool.usdc_mint @ PoolError::WrongMint)]
    pub usdc_mint: Box<Account<'info, Mint>>,
    /// Circle's new message account: a fresh keypair.
    #[account(mut)]
    pub message_sent_event_data: Signer<'info>,
    /// CHECK: the instructions sysvar (a Stellar owner's Ed25519 check).
    #[account(address = solana_sdk_ids::sysvar::instructions::ID)]
    pub instructions: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub circle: CircleBurn<'info>,
}

/// Burns everything bridgeable in `from` home to the owner (standard finality). The owner signs
/// which account to empty; where it goes is fixed by who they are.
pub fn cctp_send_home(
    mut ctx: Context<CctpSendHome>,
    nonce: u64,
    expires_at: i64,
    signature: OwnerSignature,
) -> Result<()> {
    let a = &ctx.accounts;
    let hash =
        solana_sha256_hasher::hashv(&[ix::CctpSendHome::DISCRIMINATOR, a.from.key().as_ref()])
            .to_bytes();
    let account_key = a.account.key();
    let instructions = a.instructions.to_account_info();
    let a = &mut ctx.accounts;
    authorize(
        &mut a.account,
        &account_key,
        &instructions,
        nonce,
        expires_at,
        &signature,
        &hash,
    )?;
    let scale = c::bridge_scale(a.usdc_mint.decimals, c::CANONICAL_DECIMALS).core()?;
    let amount = c::bridgeable(a.from.amount, scale).core()?;
    require!(amount > 0, PoolError::CctpNothingToSend);
    // Only the held account's USDC is waiting to be deployed; emptying another one leaves it waiting.
    let held =
        Pubkey::find_program_address(&[SEED_CCTP_HELD, a.identity.key().as_ref()], &crate::ID).0;
    let from_held = a.from.key() == held;
    let account = &mut a.account;
    if from_held {
        account.to_deploy = 0;
    }

    let domain = account.home_domain;
    let mut data = Vec::new();
    match domain {
        c::domain::ETHEREUM => {
            data.extend_from_slice(&c::DISC_DEPOSIT_FOR_BURN);
            (
                amount,
                domain,
                account.home_owner,
                [0u8; 32],
                c::OUTBOUND_MAX_FEE,
                c::FINALITY_STANDARD,
            )
                .serialize(&mut data)?;
        }
        c::domain::STELLAR => {
            let forwarder = a.pool.stellar_forwarder;
            require!(forwarder != [0u8; 32], PoolError::CctpUnsupportedDomain);
            data.extend_from_slice(&c::DISC_DEPOSIT_FOR_BURN_WITH_HOOK);
            (
                amount,
                domain,
                forwarder,
                forwarder,
                c::OUTBOUND_MAX_FEE,
                c::FINALITY_STANDARD,
            )
                .serialize(&mut data)?;
            c::forward_hook(&account.home_owner)
                .to_vec()
                .serialize(&mut data)?;
        }
        _ => return err!(PoolError::CctpUnsupportedDomain),
    }
    let circle = &a.circle;
    let identity = a.identity.key();
    let pool_key = a.pool.key();
    let (mode, domain_be, bump) = (
        [account.mode],
        account.home_domain.to_be_bytes(),
        [account.identity_bump],
    );
    let seeds = identity_seeds(&pool_key, &mode, &domain_be, &account.home_owner, &bump);
    invoke_as(
        CCTP_TOKEN_MESSENGER_MINTER_ID,
        &[
            (a.identity.to_account_info(), false),
            (a.payer.to_account_info(), true),
            (circle.sender_authority.to_account_info(), false),
            (a.from.to_account_info(), true),
            (circle.denylist_account.to_account_info(), false),
            (circle.message_transmitter.to_account_info(), true),
            (circle.token_messenger.to_account_info(), false),
            (circle.remote_token_messenger.to_account_info(), false),
            (circle.token_minter.to_account_info(), false),
            (circle.local_token.to_account_info(), true),
            (a.usdc_mint.to_account_info(), true),
            (a.message_sent_event_data.to_account_info(), true),
            (circle.message_transmitter_program.to_account_info(), false),
            (
                circle.token_messenger_minter_program.to_account_info(),
                false,
            ),
            (a.token_program.to_account_info(), false),
            (a.system_program.to_account_info(), false),
            (
                circle.token_messenger_event_authority.to_account_info(),
                false,
            ),
            (
                circle.token_messenger_minter_program.to_account_info(),
                false,
            ),
        ],
        &identity,
        data,
        &seeds,
    )?;
    emit!(CctpSentHome {
        identity,
        home_domain: domain,
        amount
    });
    Ok(())
}
