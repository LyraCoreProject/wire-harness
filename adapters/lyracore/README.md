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
| `wire-suite.sh` | The full regression suite. Pass test names as arguments, or set `WS_ONLY`, for a subset. |
| `test-*.sh` | One scenario each. |

## Running one

```sh
LYRACORE_DIR=~/src/LyraCore bash adapters/lyracore/test-cast-flow.sh
```

Against a pinned release of the client instead of a local build:

```sh
LYRACORE_DIR=~/src/LyraCore WIRE_BIN=/path/to/vanilla-wire bash adapters/lyracore/wire-suite.sh
```

The bank flow is an attended Operator check and is excluded from `wire-suite.sh`. It deletes and
recreates the named Character, moves its starter item through bank slot 39, and spends 1,000 copper:

```sh
printf '%s\n' "$PASSWORD" | LYRACORE_DIR=~/src/LyraCore \
  bash adapters/lyracore/test-bank-flow.sh TEST Banktester 2455
```

Replace `2455` with a creature template that has `UNIT_NPC_FLAG_BANKER`. The script refuses a
missing password, malformed identity, unsafe database name, missing template, or non-banker
template before it changes Character state.

The dive flow is another attended Operator check. Supply a location backed by an imported liquid
terrain cell and a disposable Character name that starts with `Dive`:

```sh
printf '%s\n' "$PASSWORD" | LYRACORE_DIR=~/src/LyraCore \
  bash adapters/lyracore/test-dive-flow.sh TEST Diveprobe 0 -9000 -400 20 10
```

The coordinates above show the argument shape only. The Operator must select and verify the real
surface and submerged heights. The adapter refuses missing or mismatched terrain before it changes
the Character.

## Operator gate

These open real sessions against a real server. In an attended session they can collide with
someone's play session — LyraCore's convention is that live wire tests are operator-gated. Ask
before running one against a stack you do not own.

The suite resolves scenario files from the adapter directory. Exit codes 126 and 127 count as
startup failures, separately from failed assertions. An unknown test name exits before fixture
setup. Subsets retain suite order, so the Transfer crash matrix still runs last.

Run `bash tests/lyracore-suite.sh` for the offline dispatch check. It uses temporary scripts and
starts no Realm.
