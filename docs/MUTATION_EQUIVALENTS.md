# Mutation testing: mutants no test can catch

`cargo-mutants` reports a mutant as missed when every test still passes. Each miss in
[TESTING.md](TESTING.md) §5 either got a test that fails on it, or is listed here with the reason
it cannot change what the code does. A mutant listed here behaves exactly like the original code
on every input, so no test could tell the two apart.

| Mutant | Why it behaves the same |
|---|---|
| `crates/pool-core/src/cctp.rs:151:39: replace + with - in parse_inbound` | `burn::VERSION` is `0`, so `header::BODY + burn::VERSION` and `header::BODY - burn::VERSION` are the same offset. |
| `crates/pool-core/src/cctp.rs:238:26: replace \| with ^ in stellar_strkey` | `acc << 8` has its low 8 bits clear and `u32::from(b)` sets only those bits, so OR and XOR give the same value. |
| `crates/pool-core/src/leg.rs:36:20: replace \|\| with && in harvest_collateral` | `value == 0` makes `growth` 0 (growth is `value - book`, saturating), so the only case the two conditions split is `growth == 0` with `value > 0`. There the original returns 0 and the mutant returns `growth * deployed / value`, which is also 0. |
| `crates/pool-core/src/upkeep.rs:13:30: replace < with <= in watch_places` | The two differ only at `c == head`, and there both map `c` to `head`. |
| `programs/safu_pool/src/leg.rs:82:9: replace \|\| with && in harvestable` | The two differ only when exactly one of `deployed_collateral` and `deployed_book` is 0. With no collateral the value is 0, so growth is 0; with no book the daily growth limit is 0. Either way `harvestable` returns 0, as the original does. |
| `programs/safu_pool/src/leg.rs:83:9: replace \|\| with && in harvestable` | The two differ only when `last_harvest_at` is 0 while money is deployed. Every deposit goes through `record_deposit`, which starts that clock first, so the state cannot occur. |
| `programs/safu_pool/src/instructions/claim.rs:261:18: replace > with >= in create_place` | Differs only when the address already holds at least the rent: the mutant then sends a 0-lamport transfer, which the System Program accepts and which changes nothing. |
| `programs/safu_pool/src/instructions/upkeep.rs:143:9: replace Snap::accounts_needed -> usize with 0` | The up-front count is a shortcut: each place is also taken with `places.next().ok_or(UpkeepAccountsMissing)`, so too few accounts fail with the same error, and the transaction reverts either way. |
| `programs/safu_pool/src/instructions/upkeep.rs:143:57: replace + with * in Snap::accounts_needed` | Same as the line above: a smaller count only moves the same refusal to the per-place check. |
| `programs/safu_pool/src/instructions/upkeep.rs:387:45: replace && with \|\| in watch_step` | The extra writes happen only when nothing was expired in this step, so the pool copy, claim and stake written back are the values just loaded: the account data does not change. |

## Demo-only values

Several clocks in `params.rs` hold two values, `clock(normal, demo)`. A mutant of the demo value
changes nothing in the normal build, and the reverse holds on the demo build. Those mutants run on
the demo build (`mutants-check-demo-<n>`), where `tests/pinned_values.rs` checks the demo values.
