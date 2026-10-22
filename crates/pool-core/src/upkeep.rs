//! Where upkeep looks (multichain `upkeep`): every user transaction also checks the head of the claim
//! line and a couple of watch-list places, so nothing waits on a keeper. Clients pass the accounts
//! for exactly these places.

use crate::params::UPKEEP_WATCH_PER_CALL;

/// The watch-list places to check: from `cursor` on, wrapping to the head, each at most once.
pub fn watch_places(head: u64, tail: u64, cursor: u64) -> [Option<u64>; UPKEEP_WATCH_PER_CALL] {
    let mut out = [None; UPKEEP_WATCH_PER_CALL];
    if head >= tail {
        return out;
    }
    let norm = |c: u64| if c < head || c >= tail { head } else { c };
    let mut pos = norm(cursor);
    for i in 0..UPKEEP_WATCH_PER_CALL {
        if out[..i].contains(&Some(pos)) {
            break;
        }
        out[i] = Some(pos);
        pos = norm(pos + 1);
    }
    out
}
