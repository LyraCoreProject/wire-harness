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
`wire-client` enables serde_json's `float_roundtrip` feature itself. Reading a trace preserves
each serialized `f64` value, including decoded `f32` coordinates widened to `f64`; callers need
no extra feature to avoid differences caused by rounding.

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

The command prints a JSON `Comparison` and returns failure if any rule differences exist, including
Deliberate Deviations. Each difference has a classification, checkpoint, stream, packet index,
opcode, field path, rule, reason and scrubbed values where available. Rust callers use
`compare::compare` and can serialize the same report for a Conformance Audit. A different scenario
revision fails comparison.
A candidate's stream labels cannot change the scenario's opcode classification.

`Comparison.differences` contains both classes. `DifferenceClass::Rule` serializes as `"rule"`;
`DifferenceClass::Data` serializes as `"data"`. Consumers must use this tag for classification.
`rule_difference_count()` and `data_difference_count()` count raw records. `is_match()` means zero
rule differences. Equal data fields emit no record. A changed data field emits one record at its
declared path, including when that path names a whole catalogue. For example:

```json
{
  "classification": "data",
  "checkpoint": "flight",
  "stream": { "kind": "broadcast", "id": "zones" },
  "packet_index": 1,
  "opcode": 756,
  "field_path": ["sound"],
  "rule": "zone_weather_sound",
  "reason": "The weather sound id follows the independently selected weather type and intensity.",
  "expected": { "type": "unsigned", "value": 0 },
  "actual": { "type": "unsigned", "value": 8533 },
  "deviations": []
}
```

The grouping signature is `(rule, checkpoint, stream, opcode, field_path)`. It excludes packet
indices and values, so repeated occurrences can share a signature. A data rule with a missing
field, incompatible shape, GUID or timestamp still produces a `"rule"` record. Packet presence,
order, timing and protocol constraints also remain rule checks. Deliberate Deviations annotate
only rule records; a declaration that matches only data is unused and fails comparison.

Stream comparisons visit checkpoints in scenario order, then streams and scoped field names in sorted order.
An aligned or missing packet uses its reference stream index; an extra packet uses its candidate
stream index. For scoped VALUES, the index is within that field's sequence and the record names
the field path. The same trace pair produces the
same record order, including ambiguous LCS ties.

The scenario TOML owns all Equivalence Rule parameters, so its SHA-256 covers them. Callers cannot
supply a replacement tolerance or random range to `compare`. General rule kinds stay in the library:

- Nonzero reference GUIDs map bijectively to candidate GUIDs across the whole trace. Zero maps only
  to zero. A nonzero Character GUID in each header establishes the first pair; aligned fields
  establish further mappings. Opening packets cannot establish mappings.
- `time_ms` fields use the scenario's inclusive `time_tolerance_ms` for comparable elapsed times.
- `wall_clock_ms` fields, including `SMSG_QUERY_TIME_RESPONSE`, must be within the scenario's
  `wall_clock_tolerance_ms` of their own trace's `recorded_at_unix_ms`. Recordings on different days
  can match. The tolerance must cover timestamp rounding and the scenario's capture duration.
- Replies keep order and multiplicity within a checkpoint. Each broadcast stream keeps its own
  order; unrelated streams can interleave. An LCS aligns packets by opcode and mapped subject GUID.
  Subject fields are `guid`, `caster`, `attacker`, or the GUID of a scoped object update.
  Unmapped subjects can establish a bijection on their first aligned pair. Equal fields break ties
  between repeated keys, then a fixed index preference breaks remaining ties. Fields compare only
  on aligned pairs. Every unmatched packet produces one `missing_packet` or `extra_packet` record.
  Object lists with subject GUIDs also align; unequal-length unkeyed lists align equal items.
  Equal-length unkeyed lists compare by index, so a changed spline vertex retains its field path.
  Two previously unmapped subjects have no cross-recording identity evidence. For example,
  reference GUIDs `[10, 20]` and candidate GUIDs `[200, 100]`, with identical non-GUID fields,
  align by stream order and establish `10 -> 200`, `20 -> 100`. The apparent swap alone is not a
  difference. A later use must keep those pairs. A mapping from an earlier checkpoint or stream
  constrains alignment and makes a swap observable. Alignment snapshots the mappings before each
  stream or list; field comparison then checks every selected pair against the growing bijection.
- Every undeclared opcode is an `unexpected_packet` difference, even if both traces contain it.
  A named `rules.unexpected_packets` entry may excuse that opcode in one checkpoint. The packet
  remains recorded, and the rule must not name a declared reply or broadcast. The connection
  scenario names ambient `SMSG_MONSTER_MOVE` as independent of its ping and name queries.
- Random fields require a name, reason, opcode, path, numeric type and inclusive range. Both values
  must have that type and fall in the range. Random rules cannot override GUID or timestamp types.
  An unused rule is a reported difference; invalid or duplicate rules fail scenario parsing.
- Named `identities` rules map unsigned 32-bit identifiers bijectively per rule, including zero.
  They cover server-local counters such as spline ids; they cannot override GUID or time fields.
  A single sample checks the unsigned 32-bit shape only. A rule's reason must state that identity
  consistency is unchecked when the scenario has one sample. Golden Trace validation counts
  declared field occurrences after excluding opening and out-of-scope packets. With one occurrence,
  it requires `identity consistency is unchecked: single sample` in the reason, ignoring case.
- Named `clocks` rules compare unsigned 32-bit monotonic timestamps relative to their first sample,
  with wraparound and their own required `tolerance_ms`. The origin is unobservable between server
  processes. A single sample checks the unsigned 32-bit shape only; its reason must say that elapsed
  clock consistency is unchecked. The required text is
  `clock consistency is unchecked: single sample`, ignoring case. The recorder and
  `check_character_observations` enforce the same sample checks. Multiple samples need no
  single-sample notice. The candidate cannot change which checks the Golden Trace promises.
- Named `packet_timing` rules compare elapsed packet arrival times from the step's send, with their
  own inclusive tolerance. An optional `path` restricts a rule to packets containing that field;
  the longest matching path takes precedence over the whole-opcode rule. This gives the taxi
  passenger's zone-driven `PLAYER_FLAGS` a polling bound without relaxing fare delivery.
  Missing arrival times fail. Unused rules fail unless an unmatched packet
  already accounts for their absence. Field-rule suppression requires that the unmatched packet
  actually contains the named path; a misspelled path still fails.
  `values_only = true` restricts a field timing rule to an `SMSG_UPDATE_OBJECT` path of the form
  `objects.<index>.values.<field>` whose object has `kind == 0`. CREATE updates keep the whole-opcode
  bound even when they contain the same field. The taxi zone flag rule uses this restriction.
- A named `character_scope` rule selects packets by explicit decoded subject paths. Packets naming
  the Character remain; packets naming only other entities are excluded from comparison. For object
  VALUES updates, comparison retains every field. It separates each field's updates into an ordered
  sequence and preserves the original `received_after_ms` on every update. Packet batching may vary;
  field multiplicity and any `packet_timing` bound still apply. A delayed fare therefore fails even
  when the final balance matches. Other updates naming the Character remain strict.
  Opening, malformed, unknown-checkpoint and mislabeled packets cannot bypass their normal checks.
  Every original packet remains in the trace. This scopes a Behavior's outcome when nearby entities
  differ with world data.
- Named `data_fields` rules classify changed values as data only at their declared opcode and field
  path. Each needs a reason. Presence, scalar types and object keys still compare; catalogue lists may differ in
  length; when both lists have items, their item shapes must match. These rules cannot cover a GUID or
  either timestamp type, even inside a container. They do not excuse a packet's presence, order or
  delivery. The taxi scenario uses them for `706.states`, `756.weather_type` and `756.sound`.
- Weather type 0 requires float grade 0 and unsigned sound 0. Both the decoder and comparison of
  recorded fields enforce this constraint, including when data or random rules cover those fields.
- Other values, including passenger fields and text digests, compare exactly. Missing or extra
  fields and changed types fail.

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
`differences.len()`. `is_match()` means zero rule differences, including zero Deliberate Deviations.
An unused declaration adds an `unused_deviation` difference naming it. It does not abort comparison.

## Alignment cost

Alignment uses Hirschberg's longest common subsequence, with equal fields breaking ties between
repeated keys. Each stream or list caches its pair scores when the product of the two lengths is
at most 1,048,576. On a 64-bit build the score cache uses at most 16 MiB, plus the linear LCS scratch
space. Larger inputs skip the cache and retain linear scratch space. Scores include only the GUID
mappings known before alignment, so caching does not change the selected pairs.

`tests/alignment.rs` compares 1,000 packets with 32 scalar fields each, once with distinct mapped
subjects and once with a repeated key. Each candidate omits packet 500 and changes a field in
packet 800. The tests require exactly those two records. Run release timings with:

```sh
cargo test --release --test alignment -- --nocapture --test-threads=1
```

The test prints five measurements for each case in release mode and one in debug mode. Timing
includes trace validation, scope, alignment and reporting, but excludes fixture construction.
It has no timing threshold that depends on the machine running it.

Measured on 2026-10-27 with Rust 1.93.0 on the same Linux x86-64 machine. The baseline is
`2599608` with the same benchmark file; the result below uses conformance.5. These are five serial
release runs per case on a shared development machine.

| 1,000-packet case | Before median, min to max | After median, min to max |
| --- | --- | --- |
| Repeated key | 3,095.410 ms, 2,728.544 to 5,658.210 | 1,557.862 ms, 1,478.758 to 1,659.747 |
| Distinct mapped subjects | 357.885 ms, 332.801 to 797.537 | 199.202 ms, 188.636 to 207.213 |

## Recorded evidence for conformance.5

Both Golden Traces were re-recorded on 2026-10-27. Each independent repeat recording returned
`{"differences": []}`. The Reference Server used these unchanged pins:

- `server`: `vmangos`
- `binary_release`: `latest/dev-linux-amd64-d9173ec.tar.gz`
- `binary_sha256`: `6fadf9dd692fdffccec1ad75aea15ff3e8d75b92a162f750f6affc9da6fd7d88`
- `source_commit`: `d9173eca3ea08dd985c52dabbcc826898d1d0b77`
- `database_commit`: `464179081673cfd240f7ffe7f0daf96bca0b5a70`

| Scenario | Recorded at, Unix ms | Character GUID | Packets | Scenario SHA-256 |
| --- | --- | --- | --- | --- |
| `connection_identity` | 1793104006810 | 10 | 34 | `36fc53aa842da673a2c4a91712b163b04f6bbeccb3d96730e3cacd5e90ee9f3e` |
| `taxi_single_hop` | 1793104046968 | 12 | 232 | `373ebb716ea792004c499e94f969ead95c2bd7cf4c66a119b54e5770cff3cd42` |

The connection repeat was recorded at 1793104016828 with Character GUID 11 and contained 43 packets.
The taxi repeat was recorded at 1793104156426 with Character GUID 14 and contained 241 packets.
Both use the same schema 3, provenance and respective scenario hashes as the Golden Traces.
Opening packets and nearby entities account for count differences outside the compared scope.
The comparator still checks
every passenger field and every declared zone packet.

## Recorded evidence for conformance.6

The taxi scenario uses entry 523 so its fixture cannot be confused with the imported entry 352
at Stormwind node 2. The Reference Server pins above did not change. The new taxi Golden Trace
was recorded at Unix ms 1793172492046 with Character GUID 10, 236 scrubbed packets and scenario
SHA-256 `264f82f2ae727eb2be18ca11e7d6b73aaad304c49ae31edcf8aadb072f1e095a`.
An independent recording at Unix ms 1793172620049 used Character GUID 12 and contained 242
packets. Comparing it with the Golden Trace returned `{"differences": []}`. The
`connection_identity` trace remains the conformance.5 recording.
