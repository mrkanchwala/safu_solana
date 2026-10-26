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
| `programs/safu_pool/src/leg.rs:178:23: replace < with <= in Money::deposit` | `collateral` is what klend minted; `min_collateral` is 95% of `expected`, the reserve's own rate for the same amount, read after the same refresh in the same instruction. klend mints at that rate, so `collateral == expected`, and `expected > min_collateral` for any `expected ≥ 1` (`deposit_ok` refuses a deposit that would mint 0). The mutant differs only when `collateral == min_collateral`, which needs a venue minting under its own rate. (`>` is caught: it flags every deposit.) |
| `programs/safu_pool/src/leg.rs:178:23: replace < with == in Money::deposit` | Same as the line above: `collateral` never equals or falls under `min_collateral` with klend, so the original and the mutant both stay silent. |
| `programs/safu_pool/src/leg.rs:216:37: replace < with <= in Money::push_idle` | Differs only when exactly `PUSH_MIN_CU` compute units remain at that line. Compute left at a point depends on the whole transaction and shifts with any code change, so a test cannot pin that single value; both sides of the cut are tested (`heavy_calls_harvest_at_the_site_limit_and_never_fail_at_the_default`). |
| `programs/safu_pool/src/leg.rs:299:89: replace < with <= in Money::harvest_with` | Same as the line above, with `HARVEST_MIN_CU`. For the permissionless harvest (`min_cu` 0) the two are the same: a running program always has more than 0 units left. |
| `programs/safu_pool/src/leg.rs:284:13: replace \|\| with && in Money::harvest_with` | The mutant differs only when exactly one of `deployed_collateral` and `deployed_book` is 0 (leg set, not paused). With no book, the growth limit `harvest_limit(0, elapsed)` is 0, so the next check returns 0 as the original does. Collateral 0 with book still held cannot occur: a harvest redeems `floor(growth × deployed / value)` with `growth ≤ value − book < value`, so it never takes all the collateral, and a redeem of all the collateral takes all the book (see `leg.rs:231:57` below). |
| `programs/safu_pool/src/leg.rs:231:24: replace \|\| with && in Money::try_pull` | `try_pull` is called only from `pull_cash`, after it has returned for `deployed_collateral == 0`, with `target ≥ amount − free > 0`. Both `target == 0` and `deployed_collateral == 0` are false there, so the mutant and the original both come down to `deployed_book == 0`. |
| `programs/safu_pool/src/leg.rs:231:57: replace \|\| with && in Money::try_pull` | As above, `target > 0` and `deployed_collateral > 0` there, so the mutant differs only when `deployed_book == 0` with collateral still held. That state cannot occur: deposits add to both, a harvest takes collateral but never book, and a redeem of `c` of `d` takes `floor(book × c / d)` of book, which leaves at least 1 while `c < d` and all of it when `c == d`. |
| `programs/safu_pool/src/venue.rs:35:9: replace Quote::collateral_for -> Result<u64> with Ok(0)` | `collateral_for` feeds only `min_collateral`, the 95% floor on a deposit (`leg.rs` `deposit`, `keeper_deploy`). A smaller `expected` only lowers the floor, and klend never mints under the real one (see `leg.rs:178` above), so both pass and book the collateral klend minted. |
| `programs/safu_pool/src/venue.rs:35:9: replace Quote::collateral_for -> Result<u64> with Ok(1)` | Same as the line above. |

## Demo-only values

Several clocks in `params.rs` hold two values, `clock(normal, demo)`. A mutant of the demo value
changes nothing in the normal build, and the reverse holds on the demo build. Those mutants run on
the demo build (`mutants-check-demo-<n>`), where `tests/pinned_values.rs` checks the demo values.
