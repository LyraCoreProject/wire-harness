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
A separate GM World Session uses `WIRE_GM_ACCOUNT`, `WIRE_GM_CHARACTER` and stdin password line 2.
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

Every received opcode is recorded. Undeclared packets retain no body and become `unexpected_packet`
differences unless a named scenario rule excuses them. Undecodable declared packets become
`packet_decode` differences. Compressed object updates decode to the same opcode and fields as
uncompressed updates. Missing, extra and reordered replies are reported after capture. Transport
failures and resource limits fail the run. The runner records each packet's elapsed milliseconds
since its step was sent and acknowledges valid teleport corrections for the Character.

`runner::FixtureAdapter` owns setup and cleanup. `runner::Transport` owns the wire boundary and
provides the Character GUID, creature bindings and any buffered opening observations.
`runner::run_on` takes an existing transport after setup; `runner::run` also calls the fixture
adapter and returns a `Run` with the Character GUID and observations. In-process callers use the
same typed packets, `Observation::from_frame` and independent decoder. Use `Observation::opening`
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
