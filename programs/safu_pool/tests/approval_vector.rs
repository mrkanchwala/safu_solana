//! The oracle approval bytes, pinned. The backend builds the same message off-chain; its test
//! (`safu/multichain/backend/tests/test_solana_pool.py`) asserts this same vector, so a change on
//! either side fails a test instead of failing every live claim at signature verification.

use anchor_lang::prelude::Pubkey;
use safu_pool::approval::{approval_hash, encode_message, ClaimApproval};

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
    assert_eq!(message.len(), 151);
    let hex: String = approval_hash(&message)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex,
        "3fe04ba305e72e46f8fbd1a66e1c5f634f0ab053b98309f7410e62e1a1a13105"
    );
}
