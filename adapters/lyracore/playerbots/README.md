# Playerbot load evidence

This acceptance program captures a private LyraCore fixture. The build-5875 client remains in the parent crate; this separate Cargo package owns the LyraCore table names and fixture inputs.

Build with `cargo build --locked --manifest-path adapters/lyracore/playerbots/Cargo.toml`. Run `vanilla-wire-playerbots INPUT.json NEW_EVIDENCE_DIRECTORY` after staging the declared bots. The input records the exact CLI executable and hash, private CLI configuration, loopback endpoint, Shard identity, bot GUIDs, source revisions, geometry, Wasm hash and fixture resources.

One confirmed CLI subscription supplies the initial state and complete transactions for the bot, runner and scheduler tables. The program waits for every declared bot to appear in a scheduled pass, then records at least sixty seconds. It saves raw transaction lines, host receive times, correlated passes and both metric scrapes. Missing rows, a lost update, inconsistent scheduler membership, early disconnect and bounded-buffer exhaustion fail the capture. A failed run retains its evidence directory.

The report checks the indexed due order for each complete transaction, accounts for every deferred bot and records damage-triggered scheduling advances. It reports each bot's pass count and lag, the oldest deferred bot, new movement route work and completed movement durations. Movement must change position. Combat and Quest credit use the runner's authoritative progress fields. Accepted actions and resolved casts without an observed effect do not count. Every processed bot contributes a route sample; retained route work contributes zero.

Every runner transaction advances the observation baseline, including updates between scheduled passes and before the measurement window. Movement duration requires the same generation, map, instance, candidate, start time and destination. The synthetic `fixtures/initial-transaction.json` is an actual confirmed CLI snapshot from SpacetimeDB 2.7.1 with Core `a880bd29` and Package `33999b5`. It verifies named sums such as `{"none":{}}`, which differ from positional SQL JSON output.

The input Module must include debug reducers with Package `decision_timing` enabled before warm-up. After the final metric scrape, the program captures JSON logs and joins every measured decision to its exact Character, generation and runner timestamp. Missing or duplicate timings and calls outside `tick_creatures` fail the run. Decision durations include instrumentation overhead and measure host elapsed time.

Transaction histograms provide bucket upper bounds for p50, p95 and maximum. A result in the unbounded final bucket reports null and names that limitation. The report retains committed and rolled-back reducer counts, physical row changes by table, and Module operation queue wait. The `spacetime_txn_cpu_time_sec` metric measures elapsed execution after lock wait, not operating-system CPU time. Its ratio to the widest scrape interval estimates accounted writer time. It excludes later commit callbacks and lock-release work; asynchronous reporting and boundary transactions prevent an exact occupancy claim.

The current result is a capture with measurements. It needs an observed load fixture and capacity assessment before it can set deployment capacity. Pass-count spread is reported separately from due-order fairness because damage can legitimately make a bot due sooner.

The CLI JSON input follows the pinned [SpacetimeDB 2.7.1 subscription implementation](https://github.com/clockworklabs/SpacetimeDB/blob/v2.7.1/crates/cli/src/subcommands/subscribe.rs). A real row update contributes one physical deletion and one insertion to the row counters. Metric visibility can lag a committed transaction; scrape start and end times remain in the result.

Execution timing follows the pinned [transaction metrics implementation](https://github.com/clockworklabs/SpacetimeDB/blob/v2.7.1/crates/datastore/src/locking_tx_datastore/datastore.rs). Queue wait includes all selected Module operations because its labels have no transaction type.
