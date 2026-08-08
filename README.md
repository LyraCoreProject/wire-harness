# wire-harness — a headless build-5875 (WoW 1.12.1) wire client

Speaks the real vanilla protocol — SRP6 logon, then the encrypted world session — so tests can
drive `CMSG` and assert on decoded `SMSG` without a game client in the loop. No wine, no async, no
game install: `cargo run`, a reachable server, and credentials.

It is **server-agnostic**. Anything that implements **build 5875** will do.
[LyraCore](https://github.com/LyraCoreProject/LyraCore) is simply the server it is developed
against, and everything LyraCore-specific is quarantined under `adapters/lyracore/`.

```
printf '%s' "$PASSWORD" | cargo run -- smoke \
    --host 10.0.0.5 --account TESTER --character Tester --password-stdin
```

## The boundary

This repository contains two different kinds of thing, and the whole point of the boundary is that
they never mix.

### Generic — the client (`src/`, `Cargo.toml`)

Build-5875 protocol and nothing else.

| Piece | What it is |
|---|---|
| `src/lib.rs` | The client: SRP6 logon, world handshake, framing, send/recv, char-select and in-world helpers |
| `src/cli.rs` | Endpoints (`--host`, `--logon-port`, `--world-port`) and identity (`--account`, `--character`, `--class`, `--race`, `--password-stdin`) |
| `src/values_mask.rs` | `SMSG_UPDATE_OBJECT` VALUES decoder — the harness's OWN implementation |
| `src/spatial.rs` | Map-coordinate and interest-grid math — the harness's OWN implementation |
| `src/main.rs`, `src/scenarios/` | The `smoke` command and the named protocol scenarios |
| `src/bin/bench/` | `vanilla-wire-bench`, the synthetic-player capacity ramp |

Rules this half keeps:

* **No dependency on any server crate.** Every dependency in `Cargo.toml` is an external protocol
  crate (`wow_srp`, `wow_login_messages`, `wow_world_messages`, `wow_world_base` — gtker's, and
  their `wow_` names are upstream's; never rename them). This is not a style preference: a path
  dependency on a server is exactly what makes a test client un-runnable against anything else.
* **No shared decoder with the server.** `values_mask.rs` and `spatial.rs` were once re-exports of
  LyraCore's copies. A test tool that decodes with the encoder's own logic cannot catch an encoder
  bug — the two errors cancel and a broken server reads green. Independent implementations, with
  their own unit tests, are the point.
* **No baked-in fixtures.** No default account, no default password, no default character, no
  default target name. Every one of those is an argument.
* **No password on a command line.** `--password-stdin` is required and takes the password from
  stdin, where `ps`, shell history and CI logs cannot see it. Nothing ever logs it. Scenarios that
  open a second session read that account's password from stdin line 2.
* **No project vocabulary.** Nothing here names a database, a reducer, a shard or a fixture
  character.

Building and testing the generic half needs **no server checkout of any kind** — see
[Tests](#tests).

### Server-specific — the adapters (`adapters/`)

The orchestration around the client: which accounts a particular dev stack provisions, what their
passwords are, which characters and creature entries the scenarios expect, and the steps that stage
and assert server state. None of it is protocol.

`adapters/lyracore/` is the one adapter that ships here, because this harness grew inside LyraCore
and its 47 orchestrators are the harness's own regression history. It is a worked example of what
an adapter looks like; a second server would get a sibling directory and touch nothing in `src/`.

| Piece | What it is |
|---|---|
| `adapters/lyracore/wire.sh` | **The seam.** Fixture credentials, endpoints and class default → the client's CLI |
| `adapters/lyracore/adapter-env.sh` | Resolves the two roots (this repo, and a LyraCore checkout) |
| `adapters/lyracore/scenario-lib.sh` | Shared orchestrator helpers (`spacetime sql`/`call`, fixtures, stay sessions) |
| `adapters/lyracore/test-*.sh`, `wire-suite.sh` | The orchestrators and the regression suite |

Every orchestrator calls the client as `"$WC" <account> <character> [scenario [args…]]`, where
`$WC` is `adapters/lyracore/wire.sh` (set once, in `scenario-lib.sh`). That is the only path from
server-specific orchestration into the generic client. Nothing exec's the binary directly except
`test-login-queue.sh`, which provisions its own throwaway accounts and passes its own password.

Point `WIRE_BIN` at a different build — a downloaded release, say — and the entire suite runs
against it unchanged.

The LyraCore adapter's **external requirements** (none of which apply to `cargo test`) are stated
at the top of `adapters/lyracore/adapter-env.sh` and in
[`adapters/lyracore/README.md`](adapters/lyracore/README.md): a LyraCore checkout
(`$LYRACORE_DIR`), a running SpacetimeDB node and gateway, the `spacetime` CLI, a module published
with `--features=debug_reducers`, and the fixture accounts.

#### Adapter configuration

| Variable | Meaning | Default |
|---|---|---|
| `LYRACORE_DIR` | Path to a LyraCore checkout | autodetected from `$PWD` |
| `WIRE_BIN` | Path to the `vanilla-wire` binary | `<this repo>/target/debug/vanilla-wire` |
| `WIRE_HOST` | Server host | `127.0.0.1` |
| `WIRE_LOGON_PORT` | Logon tier port | `3724` |
| `WIRE_WORLD_PORT` | World tier port override | unset (use the realm-list answer) |
| `WIRE_CLASS` | Class for characters the harness creates | `warlock` (the suite's historical default) |
| `WIRE_PASSWORD_<ACCOUNT>` | One account's password | see below |
| `WIRE_FIXTURE_PASSWORD` | Password for the `TEST*` accounts | `test123` |

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
`// Usage: vanilla-wire scenario …` comment above its function in `src/scenarios/`.

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
is the benchmark's server adapter — flagged as such in `--help`, switchable off with
`--metrics none`, and separate from the load generation, which is pure protocol.

## Tests

```
cargo test
```

Offline and hermetic — no server, and no server checkout, needed. Covered: CLI parsing and the
no-default/no-plaintext rules, endpoint resolution, the update-mask decoder (mask-block indexing,
packed guids, truncation safety), the spatial helpers, and framing (fragmented reads, header
cipher-state continuity, oversized/corrupt compressed frames, the crash-dump ring).

The scripts under `adapters/` are **live** tests: they need a running server and that server's
fixtures. They are not part of `cargo test` and CI never runs them.

## Build support

Build **5875** (WoW 1.12.1) only. The logon handshake, the world handshake, the opcode numbering
and the update-mask layout are all build-specific; supporting another build means another
implementation, not a flag.

## History

This repository was extracted from
[LyraCore](https://github.com/LyraCoreProject/LyraCore) (`tools/wire-client/`) with
`git filter-repo`, preserving every commit that touched those paths along with its author, date and
message. Commits before the extraction therefore describe work done inside the server repository
and reference its issue numbers.

## License

Dual-licensed under either of

* Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
* MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option. Unless you explicitly state otherwise, any contribution intentionally submitted for
inclusion in this work by you shall be dual licensed as above, without any additional terms or
conditions.
