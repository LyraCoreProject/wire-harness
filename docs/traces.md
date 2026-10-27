# Golden Traces and Equivalence Rules

Only the recorder creates Golden Traces. Record a scenario against the Reference Server, review the
scrubbed file, then commit it in the harness repository. Core implementation changes must not edit
Golden Traces. The scenario author and core implementer are separate roles for each Behavior.

```sh
WIRE_ADAPTER=vmangos vanilla-wire record scenarios/connection_identity.toml \
  "$HOME/.cache/lyracore-scratch/reference-server/provenance.json" \
  traces/connection_identity.json \
  --host 127.0.0.1 --logon-port 13724 --world-port 18085 \
  --account ACCOUNT --character Freshname --password-stdin
```

The command requires a fresh output path and a fresh Character. It verifies the scenario and
fixtures before recording. It accepts only the Reference Server adapter and vmangos provenance.
The setup script supplies the provenance file. It names the binary release, SHA-256 of its archive,
full source SHA and full database commit. The header also carries the scenario id, Behavior,
SHA-256 of the exact scenario file, schema version and recording time in Unix milliseconds.
A moving release label alone does not identify a recording.

`decode::Field` can hold text in memory and cannot serialize. `Trace::record` recursively replaces
every text field, including empty strings and nested fields, with SHA-256 of its UTF-8 bytes.
`RecordedField` has no plaintext variant. Numeric ids, flags, positions and other numeric fields
retain their type and value. Structural field names are decoder identifiers, never packet text.
Trace JSON rejects unknown fields, invalid digests, non-finite numbers and unpinned provenance.
The library's `Trace::write_new` writes any scrubbed trace, including a candidate run for an audit.
It never overwrites a file. A write failure can leave an incomplete new file; remove that failed
output before retrying. Neither raw frame logs nor client data belong in `traces/`.

## Comparison

```sh
vanilla-wire compare traces/connection_identity.json candidate.json [rules.json]
```

This command needs no server or credentials. It prints a JSON `Comparison` and returns failure if
any differences remain. Each difference has a checkpoint, stream, packet index, field path, rule
name, reason and scrubbed expected and actual values where available. Rust tests call
`compare::compare` and assert `Comparison::is_match()`. An audit can serialize the same result.
A different scenario id, Behavior or scenario-file digest fails comparison. Server provenance and
recording time can differ between the reference and candidate runs.

General Equivalence Rules apply as follows:

- Each nonzero reference GUID maps to exactly one candidate GUID across the whole trace, including
  nested fields and different checkpoints. The reverse mapping is also unique. Zero maps to zero.
  Each comparison starts with empty mappings.
- `time_ms` fields differ by at most `time_tolerance_ms`, inclusive, in either direction. The default
  is 100 ms. Clock fields must use comparable units and origins; the comparator does not infer clock
  offsets from a recording date.
- A checkpoint's reply stream preserves packet order and multiplicity. Each named broadcast stream
  also preserves its own order. Other broadcast streams may interleave with it and with replies.
  Missing or extra packets fail. The scenario declares which opcodes each checkpoint observes.
- A random field needs an explicit rule with a name, reason, opcode, field path, numeric type and
  inclusive range. Both runs must have that type and fall in the range. Lists and objects retain
  their structure. A rule that matches no compared field fails, as do duplicate rules.
- All other fields, including text digests, compare exactly. Missing or extra fields and changed
  types fail. There is no general ignore-field option. Additional permitted differences need a
  named rule, a reason and tests. Differences specific to LyraCore belong in its Deliberate Deviations.

A rules file for a random roll from 1 through 6 looks like this:

```json
{
  "time_tolerance_ms": 100,
  "random": [{
    "name": "six_sided_roll",
    "reason": "The server chooses the result of a roll from 1 through 6.",
    "opcode": 507,
    "path": ["result"],
    "range": {"type": "unsigned", "minimum": 1, "maximum": 6}
  }]
}
```

Paths name object fields and decimal list indices. There are no wildcards. Signed integers and
finite floats have their own range types. Random rules cannot override GUID or timestamp types.
The initial connection scenario has no random fields and uses the default rules.
