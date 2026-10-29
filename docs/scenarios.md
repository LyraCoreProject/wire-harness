# Scenarios

A scenario is TOML. It names one Behavior, Fixture Verbs, typed client packets, checkpoints and
Equivalence Rules. TOML keeps ordered steps readable in diffs and matches the Feature Ledger's
format. Unknown fields fail, so a typo cannot silently change a recording. This is the format
choice for LyraCore ADR 0005.

`schema_version = 3` requires an opening phase, 1 through 256 fixtures and 1 through 256 steps.
Identifiers use lowercase letters, digits and underscores. The first fixture must be the only
Character. Its name has 2 through 12 ASCII letters, with only the first capitalized. The adapter
requires that name to be absent and checks race, class, level and position after setup. Reset that
disposable fixture before rerunning. Accounts are provisioned separately. Credentials never
belong in a scenario.

## Named Characters

`schema_version = 4` declares 2 through 16 `[[characters]]`. Each has a lowercase `id` and a
`fixtures` array. Its first Fixture Verb is `character`; later verbs apply only to that Character.
The `race` in each Character fixture selects its faction. The adapters prepare every Character on
a separate Account. Set `WIRE_PEER_ACCOUNTS` to the additional Account names in Character order,
separated by commas. Send their passwords on stdin after the first Account password. The vmangos
adapter reads the GM password after those peer passwords. Accounts and passwords stay outside TOML.

Each step names its acting `character` and has exactly one `send` packet or `action`. A packet may
target the actor with `target = "character"`, another Character with
`target = { named_character = "watcher" }`, or one of the actor's creature fixtures. Session actions
are `connect`, `disconnect` and `reconnect`. `initially_connected = false` on a Character delays its
World Session until a `connect` step. Every checkpoint lists each Character once under
`[[steps.checkpoint.characters]]`. Each entry has its own ordered `replies`, named `broadcasts`,
field `expect` assertions and optional `creates` assertions. A `creates` name requires a CREATE
object update for that Character in that recipient's stream.

`walk` moves the actor from its current scenario position to `to` at `speed` yards per second,
at most 7. It sends start, heartbeat every 200 ms, and stop packets. The runner receives from all
connected Characters between sends. Its checkpoint window must exceed the walk duration.
The same runner accepts a virtual-clock `MultiTransport`; `receive` advances that shared clock
when it has no packet. A core tier 1 Transport can use one fixture connection per Character and
route each delivery instruction to the recipient's stream.

```toml
[[steps]]
id = "arrival"
character = "arriving"
action = { kind = "connect" }

[steps.checkpoint]
window_ms = 3000

[[steps.checkpoint.characters]]
character = "watcher"
creates = ["arriving"]
broadcasts = [{ opcode = 169, stream = "objects" }]

[[steps.checkpoint.characters]]
character = "arriving"
creates = ["watcher"]
broadcasts = [{ opcode = 169, stream = "objects" }]
```

Schema 4 traces carry `header.characters`, a map from Character id to GUID, and a `character` on
each packet. The comparator keeps a separate stream for each recipient and one GUID bijection for
the whole trace. A Character GUID first seen by one recipient must keep that mapping when another
recipient sees it. Unlisted opcodes remain errors. During `connect` and `reconnect`, undeclared
packets from the connecting Character are recorded as opening packets and excluded from comparison;
declared opcodes in that checkpoint still compare. `rules.character_scope` may set
`all_characters = true` to retain object updates for every named Character. The
`creates_only = true` option keeps CREATE kind and GUID for those Characters and excludes other
object updates from comparison. Use it when a scenario checks visibility rather than stats or
equipment. The raw scrubbed packets remain in the trace. Schema 3 files and traces keep their
existing bytes and hashes.

Random, identity, clock, data-field, packet-timing and unexpected-packet rules may name one
`character`. With no selector, a rule applies to every Character. Identity and clock origins
remain separate per recipient; GUID mapping remains shared across all recipients.

## Fixture Verbs

| Verb | Fields | Meaning |
| --- | --- | --- |
| `character` | `name`, `race`, `class`, `level`, `at` | Create the Character; `at` has map, x/y/z in yards and orientation in radians. |
| `money` | `copper` | Set the exact balance, from 0 through 2,147,483,647; at most once. |
| `known_flight_node` | `node_id` | Learn one node, from 1 through 256; no duplicates. |
| `creature` | `id`, `entry`, `offset` | Spawn a creature at a signed forward offset in yards, at most 100 from the Character. |

Creature ids are unique bindings for packet targets, for example `target = { fixture = "master" }`.
`target = "character"` names the Character. An unresolved binding fails before sending its packet.
Both adapters apply Character state before spawning creatures and check each spawn's entry and
position. They reconnect after setup so the opening starts at the final login. Add other verbs
only when a scenario needs them, with both adapters.

To add a Fixture Verb for a new scenario, add one typed variant to `FixtureVerb` in
`src/scenario.rs` and its bounds in `Scenario::validate`. Apply it in
`adapters/lyracore/fixture.rs` and `adapters/vmangos/fixture.rs`; a verb is ready only when both
adapters apply and verify its effect. Add a malformed-input test in `tests/scenario.rs` and a
fixture test for each adapter. Record the scenario through `reference.sh record-checked`, then
review the scrubbed trace. Keep the verb tied to the scenario that first needs it.

The LyraCore adapter calls `debug_set_level`, `debug_teleport`, `debug_set_money`,
`debug_learn_taxi_node(character_guid: u64, node_id: u32)` and `debug_spawn_at_feet` as the Operator.
Set `WIRE_ADAPTER=lyracore`, `WIRE_REALM_URL=http://127.0.0.1:PORT`, `WIRE_SHARD` and
`WIRE_OPERATOR_TOKEN_FILE`. The Module needs `debug_reducers`; curl receives the token on stdin.
The known-node reducer is the agreed T2d interface; its live verification is pending that slice.
See [the adapter](../adapters/lyracore/README.md).

The vmangos adapter checks that the gameplay Account's global and realm security levels are zero.
A separate GM World Session uses `WIRE_GM_ACCOUNT`, `WIRE_GM_CHARACTER` and the last stdin password line.
It uses `.levelup`, `.go xyzo`, `.namego` and `.modify money`. To discover a node, it uses
`.go taxinode`, summons the Character, spawns a temporary flight master and queries its menu until
the node's bit is present. It then deletes that creature. Requested creatures use `.npc spawn add`
and are deleted after capture. Both clients finish the introductory cinematic before setup and
acknowledge setup teleports. Gameplay always uses the ordinary Account.

The commands were checked at vmangos source `d9173eca3ea08dd985c52dabbcc826898d1d0b77` in
[CharacterCommands.cpp](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Commands/CharacterCommands.cpp),
[CreatureCommands.cpp](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Commands/CreatureCommands.cpp)
and [TeleportCommands.cpp](https://github.com/vmangos/core/blob/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Commands/TeleportCommands.cpp).
Setup and recording instructions are in [adapters/vmangos/](../adapters/vmangos/README.md).

## Opening and checkpoints

`[opening].window_ms` is 1 through 30,000 ms. The adapter retains frames from final player login
through `SMSG_LOGIN_VERIFY_WORLD`; the runner records these and all subsequent frames during the
opening window. They have checkpoint `opening` and stream `opening`. Supported bodies are decoded
and text is scrubbed. Unsupported bodies are discarded, with the opcode retained. Opening packets
never take part in comparison or seed GUID mappings. Comparison starts when the first step is sent.
Opening packets after a step are invalid; the opening has the same 4,096-frame cap as a checkpoint.

Each step sends a typed packet, then receives for `window_ms`, at most 180,000 ms. `replies` lists
numeric reply opcodes in order, including repeated replies. A broadcast has an opcode and a named
stream. `expect` can require an exact decoded field value and occurrence count; its path is evaluated
after scenario scoping. The recorder checks these assertions before accepting a Golden Trace.
`replies` may be empty when a successful request has no reply. Such a step must assert a declared
broadcast field. vmangos sends a Character VALUES update, without `SMSG_BUY_BANK_SLOT_RESULT`,
when it sells a bank bag slot. It sends that result opcode for refusals.

Every received opcode is recorded. Undeclared packets normally retain no body and become `unexpected_packet`
differences unless a named scenario rule excuses them. Undecodable declared packets become
`packet_decode` differences. Compressed object updates decode to the same opcode and fields as
uncompressed updates. Missing, extra and reordered replies are reported after capture. Transport
failures and resource limits fail the run. The runner records each packet's elapsed milliseconds
since its step began and acknowledges valid teleport corrections for the Character.

An `[[rules.unexpected_packets]]` entry names a checkpoint and opcode. Set
`subject_guid_high = 61744` to excuse only packets whose decoded subject GUID has `0xF130` in
its upper 16 bits, the creature GUID type. A filtered rule decodes and records the packet fields;
missing subject GUIDs, other GUID types and malformed packets remain differences. Without this
field, the rule keeps its existing opcode-wide meaning and undeclared packet bodies stay discarded.

`runner::FixtureAdapter` owns setup and cleanup. `runner::Transport` owns the wire boundary and
provides the Character GUID, creature bindings and any buffered opening observations.
`runner::run_on` takes an existing transport after setup; `runner::run` also calls the fixture
adapter and returns a `Run` with the Character GUID and observations. In-process callers use the
same typed packets and independent decoder through `Observation::from_frame_with_rules`, passing
the scenario's unexpected-packet rules so subject-filtered exceptions can see the packet's subject.
`Observation::from_frame` passes no rules and suits only scenarios without subject filters. Use `Observation::opening`
for buffered world-entry frames, and fill `received_after_ms` for timing rules.

`compare::check_character_observations` checks checkpoint contracts for a scoped scenario.
`compare::compare_character_observations` compares with a Golden Trace. Both take the actual
Character GUID. Their original unscoped counterparts remain available. Full comparison is
specified in [traces.md](traces.md); [taxi.md](taxi.md) documents the flight scenario.

```sh
WIRE_ADAPTER=vmangos vanilla-wire scenario-file scenarios/connection_identity.toml \
  --host 127.0.0.1 --logon-port 13724 --world-port 18085 \
  --account ACCOUNT --password-stdin
```
