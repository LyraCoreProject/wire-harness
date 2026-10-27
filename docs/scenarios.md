# Scenarios

A scenario is TOML. It names one Behavior, Fixture Verbs, typed client packets and checkpoints.
TOML keeps packet fields and ordered steps readable in diffs and matches the Feature Ledger's
format. Unknown fields fail, so a typo cannot silently change a recording. This is the format
choice for LyraCore ADR 0005.

`schema_version = 1` requires one Character fixture and 1 through 256 steps. Identifiers use
lowercase letters, digits and underscores. A Character specifies race, class, level and map
position in yards, with orientation in radians. The adapter requires a fresh Character name,
applies setup, then checks the Character list and login position before the scenario starts.
Accounts are provisioned separately. Credentials never belong in a scenario.

The initial vocabulary has only `character`. Add items, money, spells, flight nodes, spawned
entities or quests when a scenario needs them. Implement each new verb in both adapters before
adding a scenario that uses it.

Each step sends a typed packet, then receives for the checkpoint's `window_ms`. `replies` lists
numeric reply opcodes in their required order, including repeated replies. Missing and extra
replies fail. A broadcast has an opcode and a named stream. Packets outside the checkpoint's
listed opcodes are outside that Behavior's observation and are not recorded. Every selected
packet must decode; unknown layouts fail. A checkpoint receives at most 4,096 frames.

`FixtureAdapter` owns setup. `Transport` owns the wire boundary. An in-process runner can reuse
`Scenario`, `ClientPacket::encode` and `decode::decode` without a socket or either server adapter.
`scenario::run_on` accepts an existing transport after setup; `scenario::run` also calls the fixture
adapter. A transport failure always fails the run.

```sh
WIRE_ADAPTER=vmangos vanilla-wire scenario-file scenarios/connection_identity.toml \
  --host 127.0.0.1 --logon-port 13724 --world-port 18085 \
  --account ACCOUNT --character Freshname --password-stdin
```

For LyraCore, set `WIRE_ADAPTER=lyracore`, `WIRE_REALM_URL=http://127.0.0.1:PORT`, `WIRE_SHARD`,
and `WIRE_OPERATOR_TOKEN_FILE`. The Module must have `debug_reducers`. The adapter calls
`debug_set_level` and `debug_teleport`. It uses curl with the Operator token on stdin.
The vmangos adapter sends `.levelup` and `.go xyzo` as Speech from a GM Account.

The vmangos commands were checked in
[`Chat.cpp`](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Chat/Chat.cpp)
and [`TeleportCommands.cpp`](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Commands/TeleportCommands.cpp)
at d9173eca.
Recordings use a Reference Server built from that pinned revision. Setup and recording instructions
are in `adapters/vmangos/`.
