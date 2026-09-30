//! Oracle claim approvals: the signed message and its on-chain check.
//!
//! The oracle signs the approval off-chain with its key. The transaction carries a native Ed25519
//! precompile instruction **immediately before** `submit_claim`; the runtime verifies the signature,
//! and this module proves it is the one we expect: exactly one signature, every offset inside that
//! same precompile instruction, signed by the pool's oracle, over exactly the message rebuilt from
//! the instruction arguments. The oracle must also sign the transaction itself (multichain keeps
//! host auth for the same reason: the claim account address is the replay guard).
//!
//! Adapted from the SAFU Credit backstop (`verdict.rs`); see `DISCLOSURE.md`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::instruction::{get_stack_height, TRANSACTION_LEVEL_STACK_HEIGHT};
use solana_instructions_sysvar::{load_current_index_checked, load_instruction_at_checked};

use crate::constants::{APPROVAL_DOMAIN, SEED_REVOKED};
use crate::errors::PoolError;

/// What the oracle attests. The program never reads a price: the oracle converts the loss to
/// lamports off-chain and the program bounds it by the tier ceiling.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct ClaimApproval {
    pub staker: Pubkey,
    /// Hash of the drain transaction (any chain), as the claim id's second half.
    pub tx_hash: [u8; 32],
    /// Lamports.
    pub entitlement: u64,
    pub tier: u8,
    pub hack_timestamp: i64,
    /// Unix seconds; the approval must land by then.
    pub deadline: i64,
}

/// `APPROVAL_DOMAIN || program id || cluster || Borsh(ClaimApproval)`. Every field is fixed-size,
/// so the Borsh encoding is plain little-endian concatenation in field order.
pub fn encode_message(program_id: &Pubkey, cluster: u8, a: &ClaimApproval) -> Vec<u8> {
    let mut m = Vec::with_capacity(APPROVAL_DOMAIN.len() + 2 * 32 + 1 + 32 + 8 + 1 + 8 + 8);
    m.extend_from_slice(APPROVAL_DOMAIN);
    m.extend_from_slice(program_id.as_ref());
    m.push(cluster);
    m.extend_from_slice(a.staker.as_ref());
    m.extend_from_slice(&a.tx_hash);
    m.extend_from_slice(&a.entitlement.to_le_bytes());
    m.push(a.tier);
    m.extend_from_slice(&a.hack_timestamp.to_le_bytes());
    m.extend_from_slice(&a.deadline.to_le_bytes());
    m
}

/// Revocation key: sha256 of the message.
pub fn approval_hash(message: &[u8]) -> [u8; 32] {
    solana_sha256_hasher::hash(message).to_bytes()
}

/// Address of the revocation record for `hash`.
pub fn revoked_address(pool: &Pubkey, hash: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[SEED_REVOKED, pool.as_ref(), hash.as_ref()], &crate::ID).0
}

// Layout of the Ed25519 precompile instruction (solana-ed25519-program 3.0.0).
const SIGNATURE_OFFSETS_START: usize = 2;
const SIGNATURE_OFFSETS_SERIALIZED_SIZE: usize = 14;
const DATA_START: usize = SIGNATURE_OFFSETS_START + SIGNATURE_OFFSETS_SERIALIZED_SIZE;
const PUBKEY_LEN: usize = 32;
const SIGNATURE_LEN: usize = 64;

fn read_u16(data: &[u8], at: usize) -> Result<u16> {
    let end = at.checked_add(2).ok_or(PoolError::MalformedEd25519Instruction)?;
    let b = data.get(at..end).ok_or(PoolError::MalformedEd25519Instruction)?;
    Ok(u16::from_le_bytes([b[0], b[1]]))
}

fn slice_at(data: &[u8], offset: u16, len: usize) -> Result<&[u8]> {
    let start = offset as usize;
    let end = start.checked_add(len).ok_or(PoolError::MalformedEd25519Instruction)?;
    Ok(data.get(start..end).ok_or(PoolError::MalformedEd25519Instruction)?)
}

/// Checks the raw data of the precompile instruction at `own_index`. Pure: no sysvar access, so
/// every malformed shape is unit-testable.
pub fn check_ed25519_data(data: &[u8], own_index: u16, expected_signer: &Pubkey, expected_message: &[u8]) -> Result<()> {
    require!(data.len() >= DATA_START, PoolError::MalformedEd25519Instruction);
    require!(data[0] == 1, PoolError::WrongSignatureCount);
    require!(data[1] == 0, PoolError::MalformedEd25519Instruction);

    let signature_offset = read_u16(data, 2)?;
    let signature_ix = read_u16(data, 4)?;
    let pubkey_offset = read_u16(data, 6)?;
    let pubkey_ix = read_u16(data, 8)?;
    let message_offset = read_u16(data, 10)?;
    let message_size = read_u16(data, 12)?;
    let message_ix = read_u16(data, 14)?;

    // Signature, key and message must all live inside this very instruction, so nothing verified
    // elsewhere in the transaction can be passed off as our approval.
    require!(
        signature_ix == own_index && pubkey_ix == own_index && message_ix == own_index,
        PoolError::OffsetsOutsideEd25519Instruction
    );
    slice_at(data, signature_offset, SIGNATURE_LEN)?;
    let pubkey = slice_at(data, pubkey_offset, PUBKEY_LEN)?;
    require!(pubkey == expected_signer.as_ref(), PoolError::WrongOracleSigner);
    require!(message_size as usize == expected_message.len(), PoolError::ApprovalMessageMismatch);
    let message = slice_at(data, message_offset, message_size as usize)?;
    require!(message == expected_message, PoolError::ApprovalMessageMismatch);
    Ok(())
}

/// Finds the precompile instruction directly before the current top-level instruction and checks it.
pub fn verify_preceding_ed25519(instructions_sysvar: &AccountInfo, expected_signer: &Pubkey, expected_message: &[u8]) -> Result<()> {
    // Top-level only: the sysvar's "current index" names the outer instruction, so a CPI caller could
    // otherwise borrow a neighbouring precompile instruction whose meaning it does not control.
    require!(get_stack_height() == TRANSACTION_LEVEL_STACK_HEIGHT, PoolError::ApprovalNotTopLevel);
    let current = load_current_index_checked(instructions_sysvar)?;
    let own_index = current.checked_sub(1).ok_or(PoolError::MissingEd25519Instruction)?;
    let ix = load_instruction_at_checked(own_index as usize, instructions_sysvar)
        .map_err(|_| PoolError::MissingEd25519Instruction)?;
    require_keys_eq!(ix.program_id, solana_sdk_ids::ed25519_program::ID, PoolError::MissingEd25519Instruction);
    require!(ix.accounts.is_empty(), PoolError::MalformedEd25519Instruction);
    check_ed25519_data(&ix.data, own_index, expected_signer, expected_message)
}
