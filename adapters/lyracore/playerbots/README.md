# Playerbot load evidence

This acceptance program captures a private LyraCore fixture. The build-5875 client remains in the parent crate; this separate Cargo package owns the LyraCore table names and fixture inputs.

Build with `cargo build --locked --manifest-path adapters/lyracore/playerbots/Cargo.toml`. Run `vanilla-wire-playerbots INPUT.json NEW_EVIDENCE_DIRECTORY` after staging the declared bots. The input records the exact CLI executable and hash, private CLI configuration, loopback endpoint, Shard identity, bot GUIDs, source revisions, geometry, Wasm hash and fixture resources.

One confirmed CLI subscription supplies the initial state and complete transactions for the bot, runner and scheduler tables. The program waits for every declared bot to appear in a scheduled pass, then records at least sixty seconds. It saves raw transaction lines, host receive times, correlated passes and both metric scrapes. Missing rows, a lost update, inconsistent scheduler membership, early disconnect and bounded-buffer exhaustion fail the capture. A failed run retains its evidence directory.

The report checks the indexed due order for each complete transaction, accounts for every deferred bot and records damage-triggered scheduling advances. It reports each bot's pass count and lag, the oldest deferred bot, new movement route work and completed movement durations. Movement must change position. Combat and Quest credit use the runner's authoritative progress fields. Accepted actions and resolved casts without an observed effect do not count. Every processed bot contributes a route sample; retained route work contributes zero.

Every runner transaction advances the observation baseline, including updates between scheduled passes and before the measurement window. Movement duration requires the same generation, map, instance, candidate, start time and destination. The synthetic `fixtures/initial-transaction.json` is an actual confirmed CLI snapshot from SpacetimeDB 2.7.1 with Core `a880bd29` and Package `33999b5`. It verifies named sums such as `{"none":{}}`, which differ from positional SQL JSON output.

The current result is a capture with behavior measurements. Decision timing, queue pressure and transaction outcomes still need their measurement slices before it can set deployment capacity. Pass-count spread is reported separately from due-order fairness because damage can legitimately make a bot due sooner.

The CLI JSON input follows the pinned [SpacetimeDB 2.7.1 subscription implementation](https://github.com/clockworklabs/SpacetimeDB/blob/v2.7.1/crates/cli/src/subcommands/subscribe.rs). A real row update contributes one physical deletion and one insertion to the row counters. Metric visibility can lag a committed transaction; scrape start and end times remain in the result.
