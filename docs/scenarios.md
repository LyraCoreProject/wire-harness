# Scenarios

A scenario is TOML. It names one Behavior, Fixture Verbs, typed client packets, checkpoints and
Equivalence Rules. TOML keeps ordered steps readable in diffs and matches the Feature Ledger's
format. Unknown fields fail, so a typo cannot silently change a recording. This is the format
choice for LyraCore ADR 0005.

## Check strictness before a PR

After recording a Golden Trace and registering it in `src/catalogue.rs`, run
`vanilla-wire strictness SCENARIO`. Add `--json` for machine-readable output. The command uses the
committed scenario and raw decoded trace; it does not connect to a server. It changes decoded
fields, packet presence and order, checkpoints, recipients, and arrival times at named timing
bounds. It sends each candidate through the conformance comparator. It injects recorded opcodes with
Character and creature subjects where the decoded packet has a GUID. It also injects declared
packets labelled opening at connection checkpoints to check the opening exemption.

The report counts each distinct candidate once, then lists detected mutations, survivors explained
by a named Equivalence Rule, and unexplained survivors. In-process callers can use
`wire_client::strictness::analyze(&GoldenTrace)`. Inspect each survivor and tighten the scenario or
rule before opening a PR.
New catalogue scenarios must have zero unexplained survivors. `vanilla-wire strictness-gate` checks
the whole catalogue against `strictness_allowlist.json`. That file records existing gaps with a
reason for each mutation id. CI checks that the entries match the current report exactly.
For pull requests, CI also rejects entries absent from the allowlist at the merge base with main.
The pull request introducing the file establishes the initial approved entries. Later additions
need a maintainer to change the base allowlist in its own reviewed pull request.
The report also names the built-in `time_tolerance_ms` and `stream_interleaving` rules.
An excusal counts only when the mutation fits the rule's opcode, field, Character, checkpoint, and
subject intent. For an unexpected-packet rule without `subject_guid_high`, the analyzer derives
allowed subject GUID high bits from packets observed under that rule in the Golden Trace. It allows
packets without a decoded subject only when the Golden Trace records one under that rule. The
report marks mutations outside that intent unexplained.

`--json` emits one object with `schema_version: 2`, `scenario`, `behavior`, `mutations`,
`detected`, `detected_mutations` as ids, `excused_by_rule` as a name-to-count map,
`excused_mutations` as name-to-id lists, `rule_intents` as concrete selector descriptions, and
`unexplained_survivors` as objects with stable `id` and short `description`. Keys and arrays have
deterministic order. Mutation ids use indices in the raw Golden Trace, so a trace revision can
change them. The measure tests this finite set of wrong results, not every possible packet change.

`schema_version = 3` requires an opening phase, 1 through 256 fixtures and 1 through 256 steps.
Identifiers use lowercase letters, digits and underscores. The first fixture must be the only
Character. Its name has 2 through 12 ASCII letters, with only the first capitalized. The adapter
requires that name to be absent and checks race, class, level and position after setup. Reset that
disposable fixture before rerunning. Accounts are provisioned separately. Credentials never
belong in a scenario.

## Named Characters

`schema_version = 4` declares 1 through 16 `[[characters]]`. Each has a lowercase `id` and a
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

## Generated variants

Keep the base TOML and Golden Trace unchanged. A companion `variants/<base-id>.toml` declares
the axes for that scenario. `cargo run --bin generate_variants` writes derived TOML to
`scenarios/generated/`; `cargo run --bin generate_variants -- --check` fails if a generated file
is stale. The catalogue test makes the same check. The generated text includes the base file's
SHA-256, and its own bytes are the `scenario_sha256` in the Golden Trace. A base edit therefore
requires new generated files and checked recordings. Do not edit generated TOML by hand.

Each `[[variants]]` has one `axis`. The generated id is `<base-id>__<axis>`. Axes use one actor
named `actor`; observer axes also use `viewer`. The actor keeps the base fixtures, packets,
checkpoints and Equivalence Rules. For an observer, the generator adds an object-update stream to
the viewer's checkpoints. If the base has a Character scope, the generated rule includes both
named Characters so the viewer can compare the actor's updates. This is the only change to an
inherited rule by default. `scope_packets` may add subject paths when a new viewer stream needs
the base Character scope. The taxi late observer declares the teleport subject at opcode 197.
The viewer's ambient creature movement stays under the base scenario's
unexpected-packet rule if one exists. Other packets remain strict.

| Axis | Declaration | Generated action |
| --- | --- | --- |
| `observer` | `viewer` Character fixture | Viewer stays connected through every step. |
| `late_observer` | `viewer`, `step`, `after_ms` | Viewer connects during that step. Its checkpoint requires the actor's CREATE. |
| `reconnect` | `step`, `after_ms` | Actor disconnects and reconnects during that step, retaining its GUID. |
| `opposite_faction` | Opposite `race`, optional `at`; `character` for schema 4 | Replays the base with that Character's race and position. Choose fixtures that accept both factions. |
| `spawn_beside_viewer` | `viewer`; base must spawn a creature | Prepares the viewer first. Opening checks require the actor's creature CREATE in the viewer's stream. |

The first, second, third and fifth axes accept schema-3 bases and derive schema 4. The faction
axis preserves the base schema. Each declaration yields one variant; axes are not combined.
The generator refuses unknown fields, duplicate axes, absent event steps and invalid event times.

Timed axes produce `[[steps.events]]` with `after_ms`, `character` and a `connect` or `reconnect`
`action`. Only packet steps accept events. Times increase and fall inside the checkpoint window.
The runner collects all connected Characters until each event, changes that World Session, then
continues to the original checkpoint deadline. It keeps declared replies and broadcasts from the
new connection in that checkpoint. Undeclared login packets use the opening stream for
`opening.window_ms` after the connection returns. Malformed declared packets remain differences.

Variant declarations may add rules where the new situation requires them. `scope_packets` adds
subject paths; timed variants accept `identities`, `clocks`, `tolerances` and `packet_timing`.
The late observer accepts `watcher_movement` and `viewer_broadcasts` for its new streams and
`exclude_self_for` to omit the viewer's own object updates. Both timed variants accept
`unexpected_packets` for subject-filtered ambient opcodes. Reconnect `replies` must retain every base reply in order;
it may add packets replayed during login. `values_only_assertions` restricts inherited object
field assertions to VALUES updates so the reconnect CREATE does not count as a second purchase
or state change. The comparator still compares the CREATE fields with the Golden Trace.
Faction variants accept `unexpected_packets` and
`unexpected_subjects`, which can add a subject selector to an inherited exception that lacks one.
A variant cannot remove an inherited rule.

The committed declarations make these comparison changes. Both taxi variants also permit
creature emotes during the node query, restricted by subject GUID type.

- The taxi late viewer connects at 40,000 ms near Sentinel Hill and sees the rider before landing.
  The viewer's own login state is outside this check. The actor's VALUES, the viewer's peer VALUES,
  and both recipients' peer CREATE fields remain compared. CREATE clocks and GUID words use
  named clock and identity rules. Position sampling allows 8 units per coordinate, corresponding to 250 ms at
  32 units per second. Elapsed spline time allows 250 ms and orientation allows 0.1 radians.
  The spline points, destination, flags, mount and duration remain checked.
- Taxi reconnect also occurs at 40,000 ms. It adds the resumed taxi packets to the reply sequence.
  Only the resumed spline's start position and remaining duration receive the 8-unit and 250-ms
  sampling bounds. The original spline stays under the base rules. The unit-flags VALUES delivery
  bound accounts for one 1000-ms zone poll. Landing and dismount allow 400 ms,
  covering 250 ms login time, a 100-ms landing tick and 50 ms delivery jitter. Fare and other
  VALUES keep their inherited timing bounds.
  The new login snapshot also includes seven allocated item GUID words, covered by identity rules.
  `exclude_create_values` names only its skill-slot fields 718 through 1101, whose order changes
  between logins. This range follows the build-5875 skill array, 128 slots of three words. It does
  not cover mount, unit flags, money, movement or any VALUES update. Skill conformance needs a
  separate scenario that compares skills by id.
- The faction variant changes the arriving human warrior to an orc. Its inherited transport-notice
  exception now decodes and requires a Character subject. A separate exception permits creature
  emotes during that connection. Peer CREATE assertions remain required.
- The bank observer adds no field exceptions. The spawn variant additionally requires exactly one
  CREATE for the banker in the viewer's opening packets and compares its GUID, kind and object type.
  The actor's opening packets bind that fixture by creature entry. Other opening fields stay
  outside comparison. Bank-open replies must name the banker identified during opening in both
  the base and generated scenarios.

Record each generated file with `adapters/vmangos/reference.sh record-checked SCENARIO OUTPUT`.
That command writes two independent scrubbed samples and compares them before retaining the
Golden Trace. Register it in `src/catalogue.rs`. The catalogue test checks generated files,
recorded hashes and membership; `strictness-gate` then measures every variant.

Core tier 1 must pin a harness revision containing these entries and add each variant id to its
Behavior's Feature Ledger `scenarios`. Add a tier 1 test for each id in
`module/tests/conformance/tier1.rs` using `run_multi_scenario`, which calls
`runner::run_multi_on`. `MultiOperatorTransport` in the adjacent `transport.rs` must prepare
one fixture connection per named Character, keep its shared virtual clock across timed
`connect` and `reconnect` events, and deliver packets to every connected recipient. The taxi
test must read the actor's schema-4 fixtures when it builds the route and flight master. The
bank test must install the friendly banker template used by the base test. Run each new tier 1
test, then `./conformance/baseline <scenario>` to add its id, harness revision, scenario hash and rule
signatures to `conformance/tier1-baseline.json`. The audit and scoreboard iterate the catalogue
after the pin and ledger update. These harness recordings make no core tier 1 pass claim.
For `spawn_beside_viewer`, core must connect the viewer before `prepare_fixtures` spawns the
actor's creatures and retain the viewer's CREATE in its opening observations. The current
`MultiOperatorTransport::prepare` prepares all fixtures before connecting Characters, so it
needs that ordering change before this axis can pass tier 1.
For the visibility faction variant, `seed_visibility_world` must accept the orc warrior fixture
and install its start items as well as the human warrior's. It currently requires both Characters
to be human warriors.

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
`creates_only = true` option keeps full CREATE updates for peer Characters and excludes own login
and other object updates from comparison. Use it when a scenario checks peer visibility. The raw
scrubbed packets remain in the trace. Schema 3 files and traces keep their existing bytes and hashes.

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
are not compared, but they can bind a fixture GUID: `bank_slot_purchase` binds its banker to the
creature first seen in the opening, so an adapter must keep opening observations. Comparison starts
when the first step is sent. A Character that connects after the first step, such as a late
observer, has its own opening; any other opening packet after a step is invalid. The opening has the
same 4,096-frame cap as a checkpoint.

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
