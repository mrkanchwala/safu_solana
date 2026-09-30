# Prior work disclosure

Built for the Colosseum hackathon (window 2026-10-06 → 12). Code and designs that existed before
the window, and what was reused from each:

| Prior work | Where | What was reused |
|---|---|---|
| SAFU Staking multichain protection pool (Soroban), live on Stellar mainnet 2026-09-29 | SAFU multichain repo | The pool rules: tier ceilings, queue, cooldown, vesting, daily caps, backers, yield split. Ported to Anchor. |
| SAFU Credit Solana programs (Anchor) | SAFU Credit repo | Solana plumbing patterns: Ed25519 oracle check, LiteSVM test harness, workspace layout. |
| Earlier SOL stake prototype (Anchor, V8 rules) | SAFU core repo | SOL-handling patterns only. |
| Loss scanner and oracle backend | Private backend | Used as is; not part of this public repo. |

`programs/safu_pool`, `crates/pool-core` and the Solana site were written for this entry, starting
2026-09-30 (before the window opened); the git history shows every date.
