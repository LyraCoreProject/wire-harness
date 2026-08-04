# vanilla-wire — a headless build-5875 (WoW 1.12.1) wire client

Speaks the real vanilla protocol — SRP6 logon, then the encrypted world session — so tests can
drive `CMSG` and assert on decoded `SMSG` without a game client in the loop. No wine, no async, no
game install: `cargo run`, a reachable server, and credentials.

It is **server-agnostic**. Anything that implements build 5875 will do; this repository's server
is simply the one it is developed against.

```
printf '%s' "$PASSWORD" | cargo run -p wire-client -- smoke \
    --host 10.0.0.5 --account TESTER --character Tester --password-stdin
```

## The boundary (issue #244)

This directory contains two different kinds of thing, and the whole point of the boundary is that
they never mix. Issue #245 splits them into two repositories; the line drawn here is the line the
split will cut along.

### Generic — the client (`src/`, `Cargo.toml`)

Build-5875 protocol and nothing else.

| Piece | What it is |
|---|---|
| `src/lib.rs` | The client: SRP6 logon, world handshake, framing, send/recv, char-select and in-world helpers |
| `src/cli.rs` | Endpoints (`--host`, `--logon-port`, `--world-port`) and identity (`--account`, `--character`, `--class`, `--race`, `--password-stdin`) |
| `src/values_mask.rs` | `SMSG_UPDATE_OBJECT` VALUES decoder — the harness's OWN implementation |
| `src/spatial.rs` | Map-coordinate and interest-grid math — the harness's OWN implementation |
| `src/main.rs`, `src/modes/` | The `smoke` command and the named protocol scenarios (#245 relocates `modes/` to `scenarios/`) |
| `src/bin/bench/` | `vanilla-wire-bench`, the synthetic-player capacity ramp |

Rules this half keeps:

* **No path dependency on any server crate.** Every dependency in `Cargo.toml` is an external
  protocol crate. This is not a style preference: a path dependency is exactly what makes the tool
  un-extractable and un-runnable against anything else.
* **No shared decoder with the server.** `values_mask.rs` and `spatial.rs` were re-exports of the
  server's `lyracore-shared` copies. A test tool that decodes with the encoder's own logic cannot
  catch an encoder bug — the two errors cancel and a broken server reads green. Independent
  implementations, with their own unit tests, are the point.
* **No baked-in fixtures.** No default account, no default password, no default character, no
  default target name. Every one of those is an argument.
* **No password on a command line.** `--password-stdin` is required and takes the password from
  stdin, where `ps`, shell history and CI logs cannot see it. Nothing ever logs it. Scenarios that
  open a second session read that account's password from stdin line 2.
* **No project vocabulary.** Nothing here names a database, a reducer, a shard or a fixture
  character.

### Project-specific — the adapters (`*.sh`, `adapters/`)

The orchestration around the client: which accounts this repository's dev stack provisions, what
their passwords are, which characters and creature entries the scenarios expect, and the
`spacetime sql` / `spacetime call` steps that stage and assert server state. All of it is
LyraCore/SpacetimeDB-specific and none of it is protocol.

| Piece | What it is |
|---|---|
| `adapters/lyracore/wire.sh` | **The seam.** Fixture credentials, endpoints and class default → the client's CLI |
| `scenario-lib.sh` | Shared orchestrator helpers (`spacetime sql`/`call`, fixtures, stay sessions) |
| `test-*.sh`, `wire-suite.sh` | The orchestrators and the regression suite |

Every orchestrator calls the client as `"$WC" <account> <character> [scenario [args…]]`, where
`$WC` is `adapters/lyracore/wire.sh` (set once, in `scenario-lib.sh`). That is the only path from
project-specific orchestration into the generic client. Nothing exec's the binary directly except
`test-login-queue.sh`, which provisions its own throwaway accounts and passes its own password.

Point `WIRE_BIN` at a different build — a downloaded `wire-harness` release, say — and the entire
suite runs against it unchanged. That is the mechanism #246 uses.

#### Adapter configuration

| Variable | Meaning | Default |
|---|---|---|
| `WIRE_BIN` | Path to the `vanilla-wire` binary | `<repo>/target/debug/vanilla-wire` |
| `WIRE_HOST` | Server host | `127.0.0.1` |
| `WIRE_LOGON_PORT` | Logon tier port | `3724` |
| `WIRE_WORLD_PORT` | World tier port override | unset (use the realm-list answer) |
| `WIRE_CLASS` | Class for characters the harness creates | `warlock` (the suite's historical default) |
| `WIRE_PASSWORD_<ACCOUNT>` | One account's password | see below |
| `WIRE_FIXTURE_PASSWORD` | Password for the `TEST*` accounts | `test123` |
| `WIRE_SEAM_PASSWORD` | Password for the `SEAMTEST*` accounts | `seamtest123` |

Fixture credentials are **defaults of the adapter, never of the client**. They describe local
throwaway accounts on a local dev server; the client itself refuses to run without being told an
account and given a password.

## Commands

```
vanilla-wire smoke --host HOST --account USER --password-stdin --character NAME
vanilla-wire scenario NAME [SCENARIO-ARGS…] --account USER --password-stdin [--character NAME]
vanilla-wire-bench [OPTIONS]        # see --help
```

`vanilla-wire --help` lists every flag. Each scenario documents its own arguments in a
`// Usage: vanilla-wire scenario …` comment above its function in `src/modes/`.

`smoke` is the generic acceptance test: logon → world handshake → character enumerate (creating
the character if absent) → enter the world → report the session guid and the number of objects
that spawned. It asserts nothing project-specific, so a green smoke means "this server speaks
build 5875 and these credentials work".

### Pointing it at another server

```
printf '%s' "$PASSWORD" | vanilla-wire smoke \
    --host wow.example.com --logon-port 3724 --world-port 8085 \
    --account TESTER --character Tester --class mage --race gnome --password-stdin
```

`--world-port` overrides whatever the realm list answers with, which is what you need when a
server advertises an address that is not routable from where the harness runs (containers, port
forwards, tunnels). Without it, the realm-list answer is honored — a stock vanilla client's
behavior.

### The benchmark

`vanilla-wire-bench` drives a ramp of synthetic players (real sessions: SRP6, encrypted world
handshake, `CMSG_PLAYER_LOGIN`, `MSG_MOVE_HEARTBEAT`, `CMSG_ATTACKSWING`) and reports
client-observed movement latency and throughput. Its inputs are documented in
`vanilla-wire-bench --help`; the password comes from stdin, and a real run demands `--label`.

Its one non-generic surface is `--metrics`/`--db`/`--witness-db`: server-side writer, transaction
and table counters, scraped from a Prometheus endpoint whose series names are SpacetimeDB's. That
is the benchmark's server adapter — flagged as such in `--help`, and separate from the load
generation, which is pure protocol.

## Tests

```
cargo test -p wire-client
```

Offline and hermetic — no server needed. Covered: CLI parsing and the no-default/no-plaintext
rules, endpoint resolution, the update-mask decoder (mask-block indexing, packed guids, truncation
safety), the spatial helpers, and framing (fragmented reads, header cipher-state continuity,
oversized/corrupt compressed frames, the crash-dump ring).

The `test-*.sh` orchestrators are live tests: they need a running server and this repository's
fixtures, and they are operator-gated in attended sessions (see `../../CLAUDE.md`).
