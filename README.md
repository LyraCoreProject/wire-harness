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
| `src/scenario.rs`, `scenarios/` | TOML scenario model, Fixture Verb trait and wire runner |
| `src/main.rs`, `src/scenarios/` | The `smoke` command and the named protocol scenarios |
| `src/bin/bench/` | `vanilla-wire-bench`, the synthetic-player capacity ramp |

Rules this half keeps:

* **No dependency on any server crate.** Protocol dependencies are the independent gtker crates.
  General-purpose libraries may handle serialization, hashing and errors. This is not a style preference: a path
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

`adapters/lyracore/wire.sh` connects to the Native Host with explicit Account credentials.
`adapter-env.sh` locates a Native Host checkout when setup needs one. See the
[adapter instructions](adapters/lyracore/README.md) for endpoints and password input.
The retired shell scenarios and playerbots observer have been removed.
The [Reference Server adapter](adapters/vmangos/README.md) prepares a pinned vmangos instance in Docker.

## Library

`wire_client` exposes `WireClient`, `Endpoints`, independent `decode` packet fields,
`values_mask`, `spatial`, `scenario`, `trace` and `compare`. `WireClient::login_at` takes explicit endpoints for callers that
connect to two servers in one process. `decode::decode(opcode, body)` accepts plaintext bodies
without a socket. Unsupported opcodes, truncated fields and trailing bytes fail.
`WireClient::recv` validates compressed streams and returns compressed object updates as
`SMSG_UPDATE_OBJECT`; raw reads retain the original opcode and body. The independent VALUES
parser observes partial masks before typed parsing, so Character fields still update when a
partial packet lacks the object context that the typed decoder needs.

The conformance runner uses this boundary. See [the scenario format](docs/scenarios.md) for
Fixture Verbs, checkpoints and in-process reuse. The binary links server setup from `adapters/`;
the library has no server-specific implementation. Packet text stays in memory until
the recorder hashes it. [Golden Traces and comparison](docs/traces.md) describes the file format,
Equivalence Rules and reusable assertion report. No server crate supplies a decoder or a fixture implementation.

## Commands

```
vanilla-wire smoke --host HOST --account USER --password-stdin --character NAME
vanilla-wire compare GOLDEN ACTUAL [RULES_JSON]
vanilla-wire record SCENARIO PROVENANCE_JSON OUTPUT --account USER --character NAME --password-stdin
vanilla-wire scenario-file PATH --account USER --character NAME --password-stdin
vanilla-wire scenario NAME [SCENARIO-ARGS…] --account USER --password-stdin [--character NAME]
vanilla-wire-bench [OPTIONS]        # see --help
```

`vanilla-wire --help` lists every flag. Each scenario documents its own arguments in a
`// Usage: vanilla-wire scenario …` comment above its function in `src/scenarios/`.

`smoke` is the generic acceptance test: logon → world handshake → character enumerate (creating
the character if absent) → enter the world → report the session guid and the number of objects
that spawned. It asserts nothing project-specific, so a green smoke means "this server speaks
build 5875 and these credentials work".

### Scripted addon and movement commands

`scenario addon-control <control_dir> <seconds>` enters the named Character and writes
`ready.json` with its GUID and process ID. The caller owns a fresh directory and atomically
publishes `command-1.json`, then consecutive ordinals. For example:

```json
{"ordinal":1,"kind":"addon","message":"EXAMPLE\trequest","reply_prefix":"EXAMPLE\treply","pause_after_send":false}
```

The command sends PARTY addon chat. `message` and `reply_prefix` are complete caller-supplied
texts; this scenario knows no server command format. `sent-1.json` records the actual opcode and
plaintext body bytes after the encrypted send succeeds. `result-1.json` records the full matching
addon reply, chat type and sender GUID. A send record alone does not prove server application.

Other commands use `kind:"move"` with `from`, `to` arrays and `speed` in yards per second,
`kind:"areatrigger"` with `trigger_id`, or `kind:"stop"`. Movement sends start, heartbeat and stop
packets at the declared speed, while receiving world updates and acknowledging worldports. The
caller supplies the observed starting position and checks authoritative arrival before issuing
more movement. Movement and AreaTrigger results mean that the packets were sent.

For lost-acknowledgement tests, `pause_after_send:true` publishes the send record and pauses before
reading another frame. The caller must terminate that process. Reconnect with a fresh directory;
the client never retries commands automatically.

The scenario accepts 1 through 3600 seconds and at most 256 commands. Each command file is limited
to 8 KiB, each evidence file to 1 MiB, and each addon message to 511 bytes without NUL. Movement
accepts at most 7 yards per second and 120 seconds per leg. Addon replies have a 60-second deadline
within the overall deadline. Malformed commands, missing replies, closed connections, and expired
deadlines fail the scenario. Existing evidence cannot be overwritten.

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

Packet-layout references for the conformance decoder are vmangos
[`Query.cpp`](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Query.cpp)
and the independent gtker build-5875 definitions in `wow_world_messages` 0.3.0.
