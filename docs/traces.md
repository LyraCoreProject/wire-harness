# Golden Traces and Equivalence Rules

Only the recorder creates Golden Traces. Record a scenario against the Reference Server, review the
scrubbed file, then commit it in the harness repository. Core implementation changes must not edit
Golden Traces. The scenario author and core implementer are separate roles for each Behavior.

```sh
WIRE_ADAPTER=vmangos vanilla-wire record scenarios/connection_identity.toml \
  "$HOME/.cache/lyracore-scratch/reference-server/provenance.json" \
  traces/connection_identity.json \
  --host 127.0.0.1 --logon-port 13724 --world-port 18085 \
  --account ACCOUNT --password-stdin
```

The command requires a fresh output path and the Character name in the scenario to be absent.
The Fixture Verb fixes the name, race, class, level and position. Both adapters use that name;
`--character`, if supplied, must match it. Account credentials stay outside the scenario.
The Reference Server adapter uses a gmlevel 0 Account for gameplay and a separate GM World Session
for setup. Its wrapper supplies both passwords through stdin. See the
[Reference Server instructions](../adapters/vmangos/README.md).

Schema 3 labels a trace's `origin.kind` as `golden` or `candidate`. A Golden Trace carries the
Reference Server's binary release, archive SHA-256, full source SHA and full database commit in
`origin.provenance`. A candidate carries its adapter name and requires no vmangos provenance.
The header also has the scenario id, Behavior, SHA-256 of the exact scenario file, schema version,
`character_guid` and `recorded_at_unix_ms`. The recorder stamps this time after fixture setup,
immediately before running the scenario. A moving release label alone does not identify a recording.
Older schemas must be recorded again; do not convert their headers by hand.

`decode::Field` holds text in memory and cannot serialize. `Trace::record` recursively replaces
every text field, including empty strings and nested fields, with SHA-256 of its UTF-8 bytes.
`RecordedField` has no plaintext variant. Numeric ids, flags and positions retain their types and
values. Structural field names are decoder identifiers, never packet text. The parser rejects
unknown fields, invalid digests, non-finite numbers and incomplete Golden Trace provenance.

The opening records every frame from the final fixture login until the first step. Opening packets
are scrubbed but are excluded from comparison and GUID mappings. They must form a prefix of the
trace. They cannot excuse a packet received after a step. Every packet has `received_after_ms`,
which is null during the opening and otherwise measures time since the current request was sent.

Every received opcode in a checkpoint window becomes an observation. Declared replies and
broadcasts go through the independent decoder. Undeclared packets use the `unexpected` stream,
retain their opcode and discard their bodies. A declared packet whose body cannot decode uses the
`malformed` stream, also without its body. Neither raw frames nor client data belong in `traces/`.

Missing, extra, reordered and malformed replies become `Difference`s. They do not abort later
checkpoints. A transport failure or a resource limit still fails the run. An empty candidate trace
is valid input and reports all missing replies. The Golden Trace recorder checks the scenario's
reply contract and refuses to write a recording with unexcused differences.

## Candidate recording and library use

```sh
WIRE_ADAPTER=lyracore vanilla-wire candidate scenarios/connection_identity.toml candidate.json \
  --host 127.0.0.1 --logon-port 3724 --world-port 8085 \
  --account ACCOUNT --password-stdin
```

Set the chosen adapter's fixture environment first. `candidate` supports every adapter registered
in `adapters/mod.rs`, writes observations for later comparison, and cannot produce a Golden Trace.
Both recording commands refuse an existing output file. A failed write can leave an incomplete
new file; remove that output before retrying.

`catalogue::ENTRIES` embeds every scenario TOML and Golden Trace JSON. `catalogue::get(id)` returns
a `GoldenTrace` that binds a parsed scenario to its recorded hash. Core can enumerate ids and load
Golden Traces through its pinned git dev-dependency without locating a harness checkout.
To compare an uncommitted recording, use `GoldenTrace::new(scenario_text, trace)`. It rejects a
candidate origin or a scenario whose id, Behavior or exact bytes differ from the recorded header.
Its scenario and trace are immutable through the comparison interface.

In-process callers use `compare_observations(&golden, &observations, recorded_at_unix_ms, &deviations)`.
For Character-scoped rules use
`compare_character_observations(&golden, &observations, character_guid, recorded_at_unix_ms, &deviations)`.
A nonzero GUID is required for both traces in that case. Callers can build observations with `Observation::from_frame` to use the same independent decoder and
opcode classification as the wire runner. This path needs no candidate server provenance.

## Comparison

```sh
vanilla-wire compare scenarios/connection_identity.toml \
  traces/connection_identity.json candidate.json [deviations.json]
```

The command prints a JSON `Comparison` and returns failure if any differences exist, including
Deliberate Deviations. Each difference has a checkpoint, stream, packet index, opcode, field path,
rule, reason and scrubbed values where available. Rust callers use `compare::compare` and can
serialize the same report for a Conformance Audit. A different scenario revision fails comparison.
A candidate's stream labels cannot change the scenario's opcode classification.

The scenario TOML owns all Equivalence Rule parameters, so its SHA-256 covers them. Callers cannot
supply a replacement tolerance or random range to `compare`. General rule kinds stay in the library:

- Nonzero reference GUIDs map bijectively to candidate GUIDs across the whole trace. Zero maps only
  to zero. Each comparison starts with empty mappings.
- `time_ms` fields use the scenario's inclusive `time_tolerance_ms` for comparable elapsed times.
- `wall_clock_ms` fields, including `SMSG_QUERY_TIME_RESPONSE`, must be within the scenario's
  `wall_clock_tolerance_ms` of their own trace's `recorded_at_unix_ms`. Recordings on different days
  can match. The tolerance must cover timestamp rounding and the scenario's capture duration.
- Replies keep order and multiplicity within a checkpoint. Each broadcast stream keeps its own
  order; unrelated streams can interleave. Missing or extra packets fail.
- Every undeclared opcode is an `unexpected_packet` difference, even if both traces contain it.
  A named `rules.unexpected_packets` entry may excuse that opcode in one checkpoint. The packet
  remains recorded, and the rule must not name a declared reply or broadcast. The connection
  scenario names ambient `SMSG_MONSTER_MOVE` as independent of its ping and name queries.
- Random fields require a name, reason, opcode, path, numeric type and inclusive range. Both values
  must have that type and fall in the range. Random rules cannot override GUID or timestamp types.
  An unused random rule is a reported difference; invalid or duplicate rules fail scenario parsing.
- Named `identities` rules map unsigned 32-bit identifiers bijectively per rule, including zero.
  They cover server-local counters such as spline ids; they cannot override GUID or time fields.
- Named `clocks` rules compare unsigned 32-bit monotonic timestamps relative to their first sample,
  with wraparound and `time_tolerance_ms`. The origin is unobservable between server processes.
- Named `packet_timing` rules compare elapsed packet arrival times from the step's send, with their
  own inclusive tolerance. Missing arrival times fail. Identity, clock and timing rules must be used.
- A named `character_scope` rule selects packets by explicit decoded subject paths. Packets naming
  the Character remain; packets naming only other entities are excluded from comparison. For object
  VALUES updates, the rule selects numeric field indices and preserves each field's ordered changes
  and multiplicity across packet batching. Other updates naming the Character remain strict.
  Opening, malformed, unknown-checkpoint and mislabeled packets cannot bypass their normal checks.
  Every original packet remains in the trace. This scopes a Behavior's outcome when nearby entities
  differ with world data; it is not a general ignore-field switch.
- Other values, including text digests, compare exactly. Missing or extra fields and changed types
  fail. There is no general ignore-field option.

For example, these parameters belong in a scenario, before its fixtures and steps:

```toml
[rules]
time_tolerance_ms = 100
wall_clock_tolerance_ms = 2000

[[rules.random]]
name = "six_sided_roll"
reason = "The server chooses a result from 1 through 6."
opcode = 507
path = ["result"]
range = { type = "unsigned", minimum = 1, maximum = 6 }
```

Paths name object fields and decimal list indices. There are no wildcards. Signed integers and
finite floats have their own range types.

## Deliberate Deviations

A caller can supply typed `DeliberateDeviation`s, each with a unique name, reason and source.
`field_value` identifies a checkpoint, opcode, field path and exact expected and actual scrubbed
values. `unexpected_packet` identifies a checkpoint and opcode. These declarations acknowledge
existing differences. They cannot change how fields compare or hide a difference.

Every matching occurrence retains its original difference and gains the deviation's name, reason
and source. `Comparison::deviation_count()` counts these differences; they also remain in
`differences.len()`. `is_match()` means zero differences, including zero Deliberate Deviations.
An unused declaration adds an `unused_deviation` difference naming it. It does not abort comparison.
