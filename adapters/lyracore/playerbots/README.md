# Playerbot load evidence

This acceptance program captures a private LyraCore fixture. The build-5875 client remains in the parent crate; this separate Cargo package owns the LyraCore table names and fixture inputs.

Build with `cargo build --locked --manifest-path adapters/lyracore/playerbots/Cargo.toml`. Run `vanilla-wire-playerbots INPUT.json NEW_EVIDENCE_DIRECTORY` after staging the declared bots. The input records the exact CLI executable and hash, private CLI configuration, loopback endpoint, Shard identity, bot GUIDs, source revisions, geometry, Wasm hash and fixture resources.

One confirmed CLI subscription supplies the initial state and complete transactions for the bot, runner and scheduler tables. The program waits for every declared bot to appear in a scheduled pass, then records at least sixty seconds. It saves raw transaction lines, host receive times, correlated passes and both metric scrapes. Missing rows, a lost update, inconsistent scheduler membership, early disconnect and bounded-buffer exhaustion fail the capture. A failed run retains its evidence directory.

The current result is a capture, not a release verdict. Scheduler fairness, actual progress, movement and decision timing still require the calculation and instrumentation slices. No measured deployment capacity is claimed by this program yet.

The CLI JSON input follows the pinned [SpacetimeDB 2.7.1 subscription implementation](https://github.com/clockworklabs/SpacetimeDB/blob/v2.7.1/crates/cli/src/subcommands/subscribe.rs). A real row update contributes one physical deletion and one insertion to the row counters. Metric visibility can lag a committed transaction; scrape start and end times remain in the result.
