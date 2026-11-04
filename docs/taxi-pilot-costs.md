# Taxi pilot cost log — 2026-11-04

The check started from harness main `53b34bdd558aae99d5e4e9da8495b9f035b5e4cc`. The base taxi
scenario still lacked the creature-subject emote rule at `nodes`. Both generated variants already
had that rule. The change moves it into the base and lets the variants inherit it.

Only opcode 259, checkpoint `nodes`, and subject GUID type `0xF130` are excused. The targeted test
also found a loose rule in the comparator: Character scoping could discard unexpected packets
before their explicit subject exception was checked. The test failed before the one-line fix and
passed afterward. Unexpected packets now remain subject to their explicit Equivalence Rules.
Character subjects, other GUID types, absent subjects and other checkpoints remain differences.

## Recording cost

Times are UTC. Recorder durations marked measured come from shell `time -p`. The first three
base attempts were not individually timed; their approximate interval is reported rather than
inventing durations. Samples are independent Character resets on the same running Reference Server.
No Reference Server reset or world re-import occurred.

| Scenario | New verbs | Checked attempts | Samples captured | Stable pair |
| --- | --- | --- | --- | --- |
| `taxi_single_hop` | None | 4 | 7 | Yes: 260 and 263 packets |
| `taxi_single_hop__late_observer` | None | 2 | 4 | Yes: 294 and 326 packets |
| `taxi_single_hop__reconnect` | None | 1 | 2 | Yes: 303 and 303 packets |

Base attempts began at 05:59:27 and the successful command finished about 06:15. This includes
short intervals between attempts. The provisioned Reference Server was started at 05:58:46.
The initial guidance and source reading preceded that timestamp and was not timed.

1. Attempt 1 failed before retaining a sample because flight contained undeclared opcode 722
   (`SMSG_PLAY_SOUND`). No sound exception was added.
2. Attempt 2 captured 239 and 253 packets. Landing differed by 187 ms; dismount fields differed by
   190 ms, beyond the existing 150 ms limits. No timing bound changed.
3. Attempt 3 captured 258 and 250 packets. The repeat had an extra weather packet. No packet-count
   exception was added.
4. Attempt 4 matched, taking 219.97 measured seconds from 06:11:20. This establishes one stable
   pair, not that the scenario is free of intermittent timing or world-event differences.

The late-observer refresh began at 06:19:13. Its first pair (326 and 294 packets) failed the
existing `viewer_teleport_clock` rule after 238.08 measured seconds. Its second pair matched in
239.43 measured seconds. The clock allowance remained unchanged.

The reconnect refresh began at 06:29:34 and matched on its first pair in 220.84 measured seconds.

Both variants must be recorded again even though their effective scenario rules stay unchanged:
the generated TOML includes the base file hash. That coupling is a substantial cost for this
seven-line base-rule addition. The recordings were not edited or converted by hand.

## Verification cost and small fixes

The first debug strictness process was stopped after approximately five minutes without a result.
A release build took 45.47 seconds; the same base check then took 70.56 seconds and reported zero
unexplained survivors across 52,969 mutations. The later comparator fix requires fresh strictness
results: the base took 85.94 seconds (52,969 mutations, 2,024 detected) and the late observer
took 154.54 seconds (64,580 mutations, 3,517 detected). Reconnect took 157.83 seconds
(65,810 mutations, 3,113 detected). All three had zero unexplained survivors. This is a
build-profile change, not a smaller mutation set.

Removing an embedded trace before re-recording caused two build attempts to fail when checked
recording deleted a failed sample. Later refreshes retain the current embedded trace and record
into a new scratch path, moving the recorder's output into place only after a matching pair.
This avoids blocking targeted builds and never manufactures a Golden Trace.

No whole-catalogue strictness or full suite was run locally. Only the changed taxi scenarios and
the targeted subject-rule test were checked. The test covers creature, Character and gameobject
subjects, a different checkpoint and missing subject fields. Full regression checks belong to CI.

No Fixture Verb or core reducer is needed for this taxi change. The next core pin must include
these recordings and the comparator fix, then regenerate and inspect the affected taxi tier 1
baselines. This work does not claim a live LyraCore audit or a tier 1 pass.

Comment review passed for the changed scenario configuration, variant declarations, comparator
line and test. No explanatory source comment was added. Existing taxi timing comments and the
`docs/taxi.md` pointer remain accurate; generated file metadata is retained.

Taxi work finished about 06:39: approximately 40 minutes elapsed since the first recorder start,
interleaved with quest implementation and checks. These are overlapping wall-clock intervals,
not additive labor estimates. The Reference Server was stopped after the last check; its cache
was retained.

The separate quest pilot and its incomplete follow-up scope are in
[PR 44](https://github.com/LyraCoreProject/wire-harness/pull/44), including its own per-scenario log.
