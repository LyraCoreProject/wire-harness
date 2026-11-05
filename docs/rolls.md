# White melee Roll Conformance

`white_melee_outcomes` uses fixed expected probabilities derived from vmangos source, with a
small diagnostic recording. The complete context, formula inputs and pinned source lines are in
[LyraCore ADR 0007](https://github.com/LyraCoreProject/LyraCore/blob/conformance/statistical-rolls-design/docs/adr/0007-statistical-roll-conformance.md).
The rate author does not fix gameplay. This harness never imports a LyraCore decoder or calculation.

The scenario's `[rolls]` declares 50,000 candidate swings per target level, 2,000 reference swings
per level and basis-point probabilities. Categories are miss, dodge, parry, block, glancing, crit,
hit. Empty hands mean unarmed main-hand white melee at ordinary cadence. Character agility is 100,
unarmed and defense skills are 300/300, and all equipment slots are empty. The targets are creature
38 at levels 60 and 63, neutral faction 7, in front of the Character. Reference pairs share
the GM Island scenario coordinates, with separate targets two yards ahead, facing the Characters.
Spawned movement is idle and starting auras are cleared. Both health pools are 1,000,000,000. No haste shortcut is used.

`reference.sh record-checked scenarios/white_melee_outcomes.toml OUTPUT` holds the shared audit
lock, prepares sixteen separate Accounts and Characters, then records once. Eight Characters per
level supply 250 swings each. An unretained start/stop swing first checks fixture readiness;
it does not supply a second rate recording. The wrapper does not take a repeat recording for a roll scenario.
It reuses a running Reference Server and stops services only when it started them. A busy lock
exits with code 3. Never stop another audit's services.

The recorder checks decoded Character fields before and after collection, including level, agility,
base crit, unarmed and defense skills, empty equipment, ordinary attack time and living participants. Target health updates project percentages: maximum 100 on the
wire. A separate GM readback of update-field index 28 confirms actual maximum health before and
after capture, without changing it during capture. The pinned source is Object.cpp:837–856 and
DebugCommands.cpp:1223–1262.
The independent decoder rejects malformed, conflicting, offhand, crushing, spell, absorb and resist
outcomes. It binds each counted attack to its Character and target. Observer copies never add to
that Character's sample. Only integer counts, five 50-swing blocks per Character, elapsed time,
classifier version and small attack start/stop witnesses enter the Golden Trace. The trace header
pins the scenario bytes and vmangos binary, source and database. Packet text uses the normal scrubber.

The small check catches a misread formula, not a subtle error. It does not estimate expected rates.
The submission comparison uses conservative two-sided equal-tail exact binomial tails against fixed
probabilities. The 42-test Bonferroni family covers seven categories, two levels and three seeds,
with false failure at most 1e-6 per submission. The numerical allowance before rejection is
`64 * n * f64::EPSILON`; relative-mode recurrence avoids factorials and underflow. No Monte Carlo
or large-count approximation is used. Zero and one probabilities are handled exactly.

Core runs two public seeds. Each main merge commit supplies a deterministic third seed after merge;
a new failure calls for revert, never retry-until-green. Fixed seed and build give a fixed verdict.
The ordinary comparator rule `white_melee_distribution` reports field `outcomes/<category>`, stream
`swings`, opcode `0x014A` and a checkpoint per level and seed slot. Numerical counts and p-values
remain outside grouping signatures. A retained signature does not limit further worsening.

Run the release controls with `cargo test --release --test rolls`. They cover hand-worked binomial
tails, boundary probabilities, independent wire classification and a two-point hit-to-crit transfer
at both target levels. The synthetic controls are not recordings or measured gameplay evidence.
Core separately proves repeatability in fresh Shards and detects a real scratch crit-chance change
through delivered packets, then restores the source.

This first slice collects full candidate counts in core Tier 1. The generic TCP comparator emits
`white_melee_sample` for a candidate trace without that collector; start/stop witnesses cannot
establish roll rates. This explicit gap prevents a short TCP audit from claiming statistical parity.
