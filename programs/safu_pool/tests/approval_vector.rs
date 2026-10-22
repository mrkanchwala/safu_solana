//! The oracle approval bytes, pinned. The backend builds the same message off-chain; its test
//! (`safu/multichain/backend/tests/test_solana_pool.py`) asserts this same vector, so a change on
//! either side fails a test instead of failing every live claim at signature verification.

use anchor_lang::prelude::Pubkey;
use safu_pool::approval::{approval_hash, encode_message, ClaimApproval};
use safu_pool::constants::APPROVAL_DOMAIN;

#[test]
fn approval_message_matches_the_backend_vector() {
    let approval = ClaimApproval {
        staker: Pubkey::new_from_array([2; 32]),
        tx_hash: [3; 32],
        entitlement: 123_456_789,
        tier: 2,
        hack_timestamp: 1_700_000_000,
        deadline: 1_700_003_600,
    };
    let message = encode_message(&Pubkey::new_from_array([1; 32]), 1, &approval);
    // Domain, then fixed-size fields: program id, cluster, staker, tx hash, entitlement, tier, two times.
    assert_eq!(message.len(), APPROVAL_DOMAIN.len() + 32 + 1 + 32 + 32 + 8 + 1 + 8 + 8);
    let hex: String = approval_hash(&message)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex,
        "63f865cd06d32c08b69c6753c27dcc1289f1d1c4e896227f17e38f26dcc4a970"
    );
}
