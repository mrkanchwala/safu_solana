# Prior work disclosure

Built for the Colosseum Crypto World's Fair (submission window 14 September to 12 October 2026).
`programs/safu_pool`, `crates/pool-core` and the Solana site in `app/` were written for this entry,
starting 2026-09-30, and the git history shows every date. Code and designs from before this entry,
and what was reused from each:

| Prior work | Where | What was reused |
|---|---|---|
| SAFU_ETH (SAFUPool V8), live on Ethereum mainnet | SAFU core repo | Design origin of the tier ceilings and streamed payouts. |
| Stellar protection pool (Soroban), built under a $30K Stellar Community Fund grant | SAFU Soroban repo | Base of the multichain pool below. |
| SAFU Staking multichain protection pool (Soroban), live on Stellar mainnet 2026-09-29 | SAFU multichain repo | The pool rules: tier ceilings, queue, cooldown, vesting, daily caps, backers, yield split. Ported to Anchor, mapped in `docs/PORT_MAP.md`. |
| SAFU Credit Solana programs (Anchor) | SAFU Credit repo | Solana plumbing patterns: Ed25519 oracle check, LiteSVM test harness, workspace layout. |
| Earlier SOL stake prototype (Anchor, V8 rules) | SAFU core repo | SOL-handling patterns only. |
| Loss scanner and oracle backend | Private backend | Extended for this entry with Ethereum claims, wrongful liquidations and Solana sandwich attacks. Not part of this public repo. |
