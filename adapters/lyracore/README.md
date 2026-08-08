# adapters/lyracore — the LyraCore/SpacetimeDB orchestrators

These are **live** tests. They are not protocol, they are not generic, and `cargo test` never runs
them. They exist here rather than in the server repository because they are the harness's own
regression history — the 47 scenarios that drove `src/scenarios/` into existence — and because they
are the worked example of what a server adapter for this client looks like.

## What they need that the generic client does not

| Requirement | Why | How to satisfy it |
|---|---|---|
| A **LyraCore checkout** | they source `scripts/import-manifest.sh`, read constants out of `gateway/src/`, and build `lyracore-gateway` for its `provision` subcommand | `export LYRACORE_DIR=/path/to/LyraCore`, or run from inside one |
| A **running LyraCore stack** | every assertion is a read of live server state | SpacetimeDB node + gateway, per LyraCore's `docs/danger-zones.md` §3 |
| The **`spacetime` CLI** on `$PATH` | `spacetime sql` / `spacetime call` are how they stage and assert | authenticated against that node |
| The module published **with `--features=debug_reducers`** | they drive `debug_*` reducers to stage fixtures, damage characters, seed spawns | LyraCore's `scripts/publish-module.sh` |
| This repository's **fixture accounts** | `TEST*`, with the passwords `wire.sh` documents | provision them on that stack |

Miss any of these and the orchestrator fails loudly at its first assertion rather than reporting a
false green — but the failure will name the missing server state, not the missing prerequisite, so
check this table first.

## Layout

| File | Role |
|---|---|
| `adapter-env.sh` | Resolves `HARNESS_DIR` (this repo) and `LYRACORE_DIR` (the server checkout), cds into the latter, and provides `wire_build`. Every orchestrator sources it first. |
| `wire.sh` | **The seam.** The only path from these scripts into the generic client: fixture credentials + endpoints → `vanilla-wire`'s CLI. |
| `scenario-lib.sh` | Shared helpers: `spacetime sql`/`call` wrappers, disposable characters, stay sessions, assertions. Source, don't run. |
| `wire-suite.sh` | The full regression suite. |
| `test-*.sh` | One scenario each. |

## Running one

```sh
LYRACORE_DIR=~/src/LyraCore bash adapters/lyracore/test-cast-flow.sh
```

Against a pinned release of the client instead of a local build:

```sh
LYRACORE_DIR=~/src/LyraCore WIRE_BIN=/path/to/vanilla-wire bash adapters/lyracore/wire-suite.sh
```

## Operator gate

These open real sessions against a real server. In an attended session they can collide with
someone's play session — LyraCore's convention is that live wire tests are operator-gated. Ask
before running one against a stack you do not own.
