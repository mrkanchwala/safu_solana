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

## Demo-only values

Several clocks in `params.rs` hold two values, `clock(normal, demo)`. A mutant of the demo value
changes nothing in the normal build, and the reverse holds on the demo build. Those mutants run on
the demo build (`mutants-check-demo-<n>`), where `tests/pinned_values.rs` checks the demo values.
