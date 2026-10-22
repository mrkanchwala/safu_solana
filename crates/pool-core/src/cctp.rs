//! CCTP v2: staking and backing from Ethereum and Stellar.
//!
//! The same message layout and owner mapping as the multichain CCTP contracts
//! (`safu_multichain/contracts/cctp-common`), read from plain bytes. Circle's own program checks
//! the attestation and everything else in the message; SAFU reads only the fields it acts on.
//!
//! Decimals (KupiaSec #4): CCTP moves amounts at Circle's canonical 6 decimals. A local mint with
//! fewer decimals than that cannot hold a canonical amount exactly, so [`bridge_scale`] refuses it
//! instead of underflowing the scale.

use crate::{CoreError, Result};

/// Circle CCTP domain ids.
pub mod domain {
    pub const ETHEREUM: u32 = 0;
    pub const SOLANA: u32 = 5;
    pub const STELLAR: u32 = 27;
}

/// Home chains a CCTP staker or backer can come from.
pub const SUPPORTED_HOME_DOMAINS: [u32; 2] = [domain::ETHEREUM, domain::STELLAR];

/// `MessageV2.version` and `BurnMessageV2.version`.
pub const MESSAGE_VERSION: u32 = 1;
pub const BURN_MESSAGE_VERSION: u32 = 1;

/// Standard (finalized) transfer: outbound burns always use it.
pub const FINALITY_STANDARD: u32 = 2000;

/// Circle's canonical USDC decimals: every CCTP amount is in these units.
pub const CANONICAL_DECIMALS: u8 = 6;

/// Byte offsets inside a `MessageV2` header.
pub mod header {
    pub const VERSION: usize = 0;
    pub const SOURCE_DOMAIN: usize = 4;
    pub const DESTINATION_DOMAIN: usize = 8;
    pub const NONCE: usize = 12;
    pub const SENDER: usize = 44;
    pub const RECIPIENT: usize = 76;
    pub const DESTINATION_CALLER: usize = 108;
    pub const BODY: usize = 148;
}

/// Byte offsets inside a `BurnMessageV2` body, from the body start.
pub mod burn {
    pub const VERSION: usize = 0;
    pub const BURN_TOKEN: usize = 4;
    pub const MINT_RECIPIENT: usize = 36;
    pub const AMOUNT: usize = 68;
    pub const MESSAGE_SENDER: usize = 100;
    pub const HOOK_DATA: usize = 228;
}

/// Leading zero bytes of an EVM address left-padded to 32 bytes.
pub const EVM_PADDING: usize = 12;

/// Who controls a CCTP stake: the key that burned USDC on the home chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    /// 20-byte EVM address (checked by secp256k1 recovery).
    Evm([u8; 20]),
    /// 32-byte Stellar Ed25519 public key (a `G...` account).
    Stellar([u8; 32]),
}

impl Owner {
    /// The home chain's CCTP domain.
    pub fn domain(&self) -> u32 {
        match self {
            Owner::Evm(_) => domain::ETHEREUM,
            Owner::Stellar(_) => domain::STELLAR,
        }
    }

    /// The owner as a CCTP `bytes32` (EVM addresses left-padded).
    pub fn to_bytes32(&self) -> [u8; 32] {
        match self {
            Owner::Evm(a) => {
                let mut out = [0u8; 32];
                out[EVM_PADDING..].copy_from_slice(a);
                out
            }
            Owner::Stellar(k) => *k,
        }
    }

    /// Maps a burn's (source domain, message sender) to its owner. `None` for an unsupported
    /// domain, or an EVM sender that is not a left-padded 20-byte address.
    pub fn from_sender(source_domain: u32, sender: &[u8; 32]) -> Option<Owner> {
        match source_domain {
            domain::ETHEREUM => {
                if sender[..EVM_PADDING].iter().any(|b| *b != 0) {
                    return None;
                }
                let mut a = [0u8; 20];
                a.copy_from_slice(&sender[EVM_PADDING..]);
                Some(Owner::Evm(a))
            }
            domain::STELLAR => Some(Owner::Stellar(*sender)),
            _ => None,
        }
    }
}

/// The fields of an inbound burn message SAFU acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboundBurn {
    pub source_domain: u32,
    pub destination_domain: u32,
    pub nonce: [u8; 32],
    pub destination_caller: [u8; 32],
    pub mint_recipient: [u8; 32],
    pub message_sender: [u8; 32],
    pub burn_token: [u8; 32],
    /// Burned amount, canonical decimals.
    pub amount: u128,
}

fn read_u32(msg: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(msg.get(at..at + 4)?.try_into().ok()?))
}

fn read_32(msg: &[u8], at: usize) -> Option<[u8; 32]> {
    msg.get(at..at + 32)?.try_into().ok()
}

/// Parses the header and burn body. `None` if the message is too short, either version is not v2,
/// or the amount does not fit 128 bits. Circle's program validates the rest.
pub fn parse_inbound(msg: &[u8]) -> Option<InboundBurn> {
    if msg.len() < header::BODY + burn::HOOK_DATA
        || read_u32(msg, header::VERSION)? != MESSAGE_VERSION
        || read_u32(msg, header::BODY + burn::VERSION)? != BURN_MESSAGE_VERSION
    {
        return None;
    }
    let body = header::BODY;
    let amount = read_32(msg, body + burn::AMOUNT)?;
    if amount[..16].iter().any(|b| *b != 0) {
        return None;
    }
    Some(InboundBurn {
        source_domain: read_u32(msg, header::SOURCE_DOMAIN)?,
        destination_domain: read_u32(msg, header::DESTINATION_DOMAIN)?,
        nonce: read_32(msg, header::NONCE)?,
        destination_caller: read_32(msg, header::DESTINATION_CALLER)?,
        mint_recipient: read_32(msg, body + burn::MINT_RECIPIENT)?,
        message_sender: read_32(msg, body + burn::MESSAGE_SENDER)?,
        burn_token: read_32(msg, body + burn::BURN_TOKEN)?,
        amount: u128::from_be_bytes(amount[16..].try_into().ok()?),
    })
}

/// Local base units per canonical unit. Refuses a local mint with fewer decimals than canonical
/// (KupiaSec #4): the scale would underflow and amounts could not round-trip.
pub fn bridge_scale(local_decimals: u8, canonical_decimals: u8) -> Result<u64> {
    let gap = local_decimals
        .checked_sub(canonical_decimals)
        .ok_or(CoreError::DecimalsMismatch)?;
    10u64.checked_pow(u32::from(gap)).ok_or(CoreError::Overflow)
}

/// The part of `balance` an outbound burn can carry: whole canonical units. The rest stays for the
/// next send.
pub fn bridgeable(balance: u64, scale: u64) -> Result<u64> {
    if scale == 0 {
        return Err(CoreError::DivideByZero);
    }
    Ok(balance - balance % scale)
}

// -----------------------------------------------------------------------
// Sending home to Stellar. Circle's Stellar side treats `mintRecipient` as a contract, so a payout
// to a normal `G...` wallet goes through Circle's CctpForwarder: the burn names the forwarder as
// both `mintRecipient` and `destinationCaller`, and the hook data names the wallet as a strkey.
// Layout from Circle's Stellar reference: 24 zero bytes, version u32 = 0, length u32, then the
// strkey as text, all big-endian. A wrong layout strands the USDC for good, hence the vectors in
// the tests (made with stellar_sdk).

/// Length of a `G...` strkey.
pub const STELLAR_STRKEY_LEN: usize = 56;
/// Circle-reserved zero bytes at the start of forwarder hook data.
pub const FORWARD_MAGIC_LEN: usize = 24;
/// Forwarder hook data version.
pub const FORWARD_VERSION: u32 = 0;
/// Forwarder hook data for one `G...` wallet.
pub const FORWARD_HOOK_LEN: usize = FORWARD_MAGIC_LEN + 4 + 4 + STELLAR_STRKEY_LEN;

/// Strkey version byte of an Ed25519 public key (`G...`): 6 << 3.
const STRKEY_ACCOUNT_VERSION: u8 = 6 << 3;
const BASE32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

/// CRC-16/XMODEM, as strkey uses.
fn crc16_xmodem(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for b in data {
        crc ^= u16::from(*b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// A Stellar Ed25519 public key as its `G...` strkey (version byte, key, CRC16 little-endian,
/// base32 without padding: 35 bytes make exactly 56 characters).
pub fn stellar_strkey(key: &[u8; 32]) -> [u8; STELLAR_STRKEY_LEN] {
    let mut raw = [0u8; 35];
    raw[0] = STRKEY_ACCOUNT_VERSION;
    raw[1..33].copy_from_slice(key);
    let crc = crc16_xmodem(&raw[..33]);
    raw[33..].copy_from_slice(&crc.to_le_bytes());
    let mut out = [0u8; STELLAR_STRKEY_LEN];
    let (mut acc, mut bits, mut n) = (0u32, 0u32, 0usize);
    for b in raw {
        acc = (acc << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out[n] = BASE32[((acc >> bits) & 31) as usize];
            n += 1;
        }
    }
    out
}

/// Hook data that makes Circle's CctpForwarder pass a payout on to `key`'s `G...` wallet.
pub fn forward_hook(key: &[u8; 32]) -> [u8; FORWARD_HOOK_LEN] {
    let mut out = [0u8; FORWARD_HOOK_LEN];
    let mut at = FORWARD_MAGIC_LEN;
    out[at..at + 4].copy_from_slice(&FORWARD_VERSION.to_be_bytes());
    at += 4;
    out[at..at + 4].copy_from_slice(&(STELLAR_STRKEY_LEN as u32).to_be_bytes());
    at += 4;
    out[at..].copy_from_slice(&stellar_strkey(key));
    out
}

// -----------------------------------------------------------------------
// Owner-signed actions. A CCTP owner has no Solana key: they sign with their home wallet, and the
// program checks that signature before acting for them. What they sign is a 32-byte digest,
// `sha256(action_preimage(..))`, so a wallet shows one short hex string.
//
// - Ethereum: `personal_sign` of the digest (EIP-191 over 32 bytes), checked by secp256k1 recovery.
// - Stellar: SEP-53 `signMessage` of the digest, i.e. Ed25519 over
//   `sha256("Stellar Signed Message:\n" || digest)`.
//
// The preimage binds the program, the owner's account, a per-account nonce (each signature works
// once) and an expiry, plus a hash of the exact call (instruction data and accounts).

/// Domain tag at the start of every signed action.
pub const ACTION_DOMAIN: &[u8; 26] = b"SAFU_SOLANA_CCTP_ACTION_V1";
/// Byte length of [`action_preimage`].
pub const ACTION_PREIMAGE_LEN: usize = 26 + 32 + 32 + 8 + 8 + 32;
/// EIP-191 prefix of a 32-byte message.
pub const EIP191_PREFIX_32: &[u8; 28] = b"\x19Ethereum Signed Message:\n32";
/// SEP-53 prefix (Stellar signed message).
pub const SEP53_PREFIX: &[u8; 24] = b"Stellar Signed Message:\n";
/// The longest a signed action may wait before it is used.
pub const MAX_ACTION_TTL_SECS: i64 = 60 * 60;

/// What the owner's digest is `sha256` of.
pub fn action_preimage(
    program: &[u8; 32],
    account: &[u8; 32],
    nonce: u64,
    expires_at: i64,
    call_hash: &[u8; 32],
) -> [u8; ACTION_PREIMAGE_LEN] {
    let mut out = [0u8; ACTION_PREIMAGE_LEN];
    let mut at = 0;
    for part in [
        &ACTION_DOMAIN[..],
        program,
        account,
        &nonce.to_le_bytes(),
        &expires_at.to_le_bytes(),
        call_hash,
    ] {
        out[at..at + part.len()].copy_from_slice(part);
        at += part.len();
    }
    out
}

/// A signed action is usable from now until its expiry, which may not be further out than
/// [`MAX_ACTION_TTL_SECS`].
pub fn check_action_expiry(now: i64, expires_at: i64) -> Result<()> {
    if now > expires_at {
        return Err(CoreError::SignatureExpired);
    }
    if expires_at - now > MAX_ACTION_TTL_SECS {
        return Err(CoreError::SignatureDeadlineTooFar);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(version: u32, body_version: u32, len: usize) -> [u8; 512] {
        let mut m = [0u8; 512];
        m[..4].copy_from_slice(&version.to_be_bytes());
        m[4..8].copy_from_slice(&domain::ETHEREUM.to_be_bytes());
        m[8..12].copy_from_slice(&domain::SOLANA.to_be_bytes());
        m[header::BODY..header::BODY + 4].copy_from_slice(&body_version.to_be_bytes());
        m[header::BODY + 4..len].fill(0xee);
        let amount = header::BODY + burn::AMOUNT;
        m[amount..amount + 32].fill(0);
        m[amount + 31] = 42;
        m
    }

    const FULL: usize = header::BODY + burn::HOOK_DATA;

    #[test]
    fn parses_a_minimal_v2_burn() {
        let m = message(MESSAGE_VERSION, BURN_MESSAGE_VERSION, FULL);
        let b = parse_inbound(&m[..FULL]).unwrap();
        assert_eq!(
            (b.source_domain, b.destination_domain, b.amount),
            (domain::ETHEREUM, domain::SOLANA, 42)
        );
        assert_eq!(b.message_sender, [0xee; 32]);
        assert_eq!(b.burn_token, [0xee; 32]);
    }

    #[test]
    fn rejects_short_messages_other_versions_and_huge_amounts() {
        let m = message(MESSAGE_VERSION, BURN_MESSAGE_VERSION, FULL);
        assert!(parse_inbound(&m[..FULL - 1]).is_none());
        assert!(
            parse_inbound(&message(MESSAGE_VERSION + 1, BURN_MESSAGE_VERSION, FULL)[..FULL])
                .is_none()
        );
        assert!(
            parse_inbound(&message(MESSAGE_VERSION, BURN_MESSAGE_VERSION + 1, FULL)[..FULL])
                .is_none()
        );
        let mut big = m;
        big[header::BODY + burn::AMOUNT] = 1;
        assert!(parse_inbound(&big[..FULL]).is_none());
    }

    #[test]
    fn evm_sender_round_trips_and_padding_is_enforced() {
        let mut raw = [0u8; 32];
        raw[EVM_PADDING..].fill(0x11);
        let owner = Owner::from_sender(domain::ETHEREUM, &raw).unwrap();
        assert_eq!(owner, Owner::Evm([0x11; 20]));
        assert_eq!(
            (owner.to_bytes32(), owner.domain()),
            (raw, domain::ETHEREUM)
        );
        raw[0] = 1;
        assert!(Owner::from_sender(domain::ETHEREUM, &raw).is_none());
    }

    #[test]
    fn only_home_domains_map_to_an_owner() {
        for d in SUPPORTED_HOME_DOMAINS {
            assert_eq!(
                Owner::from_sender(d, &[0u8; 32]).map(|o| o.domain()),
                Some(d)
            );
        }
        assert!(Owner::from_sender(domain::SOLANA, &[0u8; 32]).is_none());
        assert!(Owner::from_sender(1, &[0u8; 32]).is_none());
    }

    #[test]
    fn decimals_check_refuses_fewer_local_than_canonical() {
        assert_eq!(bridge_scale(6, CANONICAL_DECIMALS), Ok(1));
        assert_eq!(bridge_scale(7, CANONICAL_DECIMALS), Ok(10));
        assert_eq!(
            bridge_scale(5, CANONICAL_DECIMALS),
            Err(CoreError::DecimalsMismatch)
        );
        assert_eq!(bridge_scale(u8::MAX, 0), Err(CoreError::Overflow));
    }

    #[test]
    fn only_whole_canonical_units_leave() {
        assert_eq!(bridgeable(1_234_567, 1), Ok(1_234_567));
        assert_eq!(bridgeable(1_234_567, 10), Ok(1_234_560));
        assert_eq!(bridgeable(9, 10), Ok(0));
        assert_eq!(bridgeable(9, 0), Err(CoreError::DivideByZero));
    }

    /// Vectors from stellar_sdk `StrKey.encode_ed25519_public_key`.
    #[test]
    fn strkey_matches_stellar_sdk() {
        let mut ordered = [0u8; 32];
        for (i, b) in ordered.iter_mut().enumerate() {
            *b = i as u8;
        }
        assert_eq!(
            &stellar_strkey(&[7; 32]),
            b"GADQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOZPI"
        );
        assert_eq!(
            &stellar_strkey(&ordered),
            b"GAAACAQDAQCQMBYIBEFAWDANBYHRAEISCMKBKFQXDAMRUGY4DUPB7JZX"
        );
    }

    #[test]
    fn forward_hook_follows_circles_layout() {
        let h = forward_hook(&[7; 32]);
        assert_eq!(h.len(), 88);
        assert_eq!(h[..24], [0u8; 24]);
        assert_eq!(h[24..28], 0u32.to_be_bytes());
        assert_eq!(h[28..32], 56u32.to_be_bytes());
        assert_eq!(
            &h[32..],
            b"GADQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOBYHA4DQOZPI"
        );
    }

    #[test]
    fn action_preimage_binds_every_field_in_order() {
        let p = action_preimage(&[1; 32], &[2; 32], 3, 4, &[5; 32]);
        assert_eq!(&p[..26], ACTION_DOMAIN);
        assert_eq!(p[26..58], [1; 32]);
        assert_eq!(p[58..90], [2; 32]);
        assert_eq!(p[90..98], 3u64.to_le_bytes());
        assert_eq!(p[98..106], 4i64.to_le_bytes());
        assert_eq!(p[106..], [5; 32]);
    }

    #[test]
    fn signed_actions_expire_and_cannot_be_long_lived() {
        assert_eq!(check_action_expiry(100, 100), Ok(()));
        assert_eq!(
            check_action_expiry(101, 100),
            Err(CoreError::SignatureExpired)
        );
        assert_eq!(check_action_expiry(0, MAX_ACTION_TTL_SECS), Ok(()));
        assert_eq!(
            check_action_expiry(0, MAX_ACTION_TTL_SECS + 1),
            Err(CoreError::SignatureDeadlineTooFar)
        );
    }
}
