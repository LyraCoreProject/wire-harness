# Direct-damage spell Roll Conformance

`spell_direct_damage_outcomes` is the second Roll Conformance slice. Its author derives rates
from pinned vmangos source and does not fix gameplay or derive expectations from LyraCore code.
The exception to recording authority covers roll rates only; see
[LyraCore ADR 0007](https://github.com/LyraCoreProject/LyraCore/blob/main/docs/adr/0007-statistical-roll-conformance.md).

## Pinned context

A level-60 human mage (race 1, class 8) casts Scorch rank 1, spell 2948. Intellect is 125.
All equipment slots are empty. There are no talents, auras, hit bonuses, crit bonuses, spell
penetration or procs. The target is creature 38, normal rank, non-pet, type 7, class 1, at level
60 or 63. Fire resistance is zero; all extra and static flags are zero. Both participants stand
still, alive, on map 1 at (16222.1, 16252.1, 12.5872). The target stands two yards ahead and faces
the Character. Each Character has a separate target. Faction 7, idle movement and passive reaction
prevent interference; these inputs do not enter the roll formulas. Starting auras are removed.
Both health pools and the Character's maximum mana are 1,000,000,000. There are no resource
restores during collection. The Reference Server retains Scorch's ordinary 1500-ms cast time and
1500-ms global cooldown. Casts start at least 1600 ms apart; cadence is outside this slice.

Scorch has one direct-damage effect, no projectile delay, periodic damage, applied aura, mechanic,
proc or cooldown beyond the global cooldown. Unlike Frostbolt, it requires no spell aura exception.
It exercises the actual level-difference hit formula and innate partial resistance without gear or
talents. Partial resistance is included because its rate can be derived exactly.

The pinned database is `464179081673cfd240f7ffe7f0daf96bca0b5a70`, archive SHA-256
`9e178f1d70363e92a7b8c259000102dc9739333d79ca00c99ea32dd1fb43e904`. Its `mangos.sql:4077`
contains human mage level-60 create stats `(1,8,60,30,35,45,125,126)`. The fixture sets the additive
intellect modifier to zero and checks delivered intellect 125. `mangos.sql:5879`, `spell_template`
entry 2948/build 5464, supplies school 2, damage class 1, attributes 65536, all extended attributes
zero, effects [2,0,0], no mechanic or aura, cast-time index 16, speed zero and start recovery 1500.
The current build selects that row. The target and its non-combat EventAI context follow the first
slice. The recorder checks its actual fire resistance and maximum health through GM readback before
and after collection. Character level, race/class, intellect, equipment, auras, mana, health, target
level/faction and stationary positions are checked through independently decoded update fields.
Invalid or incomplete context fails setup; it is not a rate finding.

## Independent derivation

All source references below use one-based lines at commit
`d9173eca3ea08dd985c52dabbcc826898d1d0b77`, fetched through raw GitHub URLs only.

- [SpellCaster.cpp:169–237](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/SpellCaster.cpp) selects the magic hit table for
  damage class 1, after evade, immunity and reflection checks. Those modifiers are absent here.
- [SpellCaster.cpp:772–880](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/SpellCaster.cpp) uses target-minus-caster level
  difference `d`, PvE coefficient 11, and base hit `96-d` percent for `d < 3`, otherwise
  `94-(d-2)*11` percent. All spell, aura, mechanic, debuff and hit modifiers are zero. Scorch is
  non-binary. Hit thresholds are 9600 and 8300. `irand(0,10000)` is inclusive and returns
  `SPELL_MISS_RESIST` for rolls below `10000-hit`. Thus full-resist probabilities are exactly
  `400/10001` and `1700/10001`; landed probabilities H are `9601/10001` and `8301/10001`.
  `SPELL_MISS_MISS` has probability zero for this context.
- [Random.cpp:28–31, 62–65](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/shared/Utilities/Random.cpp),
  [Random.h:58–62](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/shared/Utilities/Random.h), and
  [MersenneTwister.h:159–163, 202–220](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/dep/include/mersennetwister/MersenneTwister.h)
  define inclusive integer sampling and `roll_chance_f(chance)` as `chance > rand_chance()`.
  The latter compares against exactly `100*U/2^32`, with U uniform on 0 through `2^32-1`.
- [Unit.cpp:2597–2662](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/Unit.cpp) gives mage crit as
  `3.70f + intellect/(14.77f + 0.65f*level)`, with no aura bonuses. At intellect 125 and level 60,
  single-precision arithmetic gives denominator `53.77000045776367` and chance
  `6.024716377258301` percent (bits `0x40c0ca7a`, exactly `3158685/524288`). The strict comparison
  accepts `ceil(chance*2^32/100) = 258759599` integers. The exact crit probability C is therefore
  `258759599/4294967296`, rather than a rounded basis-point rate.
- [Unit.cpp:5212–5321](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/Unit.cpp) reads that school crit value, with no creature
  level suppression for magic damage. Target auras, class scripts and spell modifiers are absent.
  [Spell.cpp:900–923](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Spells/Spell.cpp) rolls hit and crit separately. Delivered crit
  probability is H*C; ordinary hit probability is H*(1-C).
  [SpellCaster.cpp:1230–1275](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/SpellCaster.cpp) carries the crit result into the delivered damage flag;
  Scorch has no ignore-caster-modifiers attribute.
- [SpellMgr.cpp:3342–3394](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Spells/SpellMgr.cpp) makes Scorch non-binary because its
  only effect is direct damage. [SpellEntry.h:506–523, 1110–1122](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Spells/SpellEntry.h)
  allows that effect to crit; the cannot-crit attribute is absent.
- [SpellCaster.cpp:882–924](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/SpellCaster.cpp) adds innate creature resistance
  `trunc(8*d*60/63)` to zero fire resistance and zero penetration. This is 0 or 22. Multiplication
  by `0.15f/60`, then by 100 in the resistance roll, yields 0 or approximately 5.50000048 percent.
- [Unit.cpp:1885–1918, 2397–2469](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/Unit.cpp) interpolates between resistance
  rows at chance 5 and 8. At level 63 the cumulative thresholds are exactly 1 for 75% resistance,
  between 5 and 6 for at least 50%, and between 18 and 19 for at least 25%. Against integer rolls
  0 through 99 this gives widths 1, 5, 13 and 81 for 75%, 50%, 25% and zero resistance. The
  conditional partial-resist probability is exactly 19/100; it is zero at level 60. Damage is
  always positive and large enough for every nonzero quarter to remain positive after dithering.
  [Unit.cpp:1920–1955, 2205–2233](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/Unit.cpp) applies this roll after a landed
  non-binary cast. Unconditional partial-resist probability is H*19/100 at level 63.
- [Spell.cpp:223–254](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Spell.cpp) defines the independently decoded
  `SMSG_SPELLNONMELEEDAMAGELOG` layout. [SpellDefines.h:190–229](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Spells/SpellDefines.h)
  defines critical flag 0x02. Positive resisted damage marks partial resistance. `SMSG_SPELL_GO`
  identifies hit targets or full-resist reason 2; reason 1 is a distinct miss. Absorb, block,
  immunity, reflection, negative resistance, other spells and changed targets fail setup.
- [UnitCommands.cpp:1502–1534, 2327–2369](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Commands/UnitCommands.cpp),
  [Unit.cpp:7890–7905](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/Unit.cpp),
  [CreatureCommands.cpp:845–872](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Commands/CreatureCommands.cpp), and
  [DebugCommands.cpp:1223–1269](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Commands/DebugCommands.cpp) define intellect, mana,
  passive reaction and private update-field readback used only during fixture preparation/checks.
  [UpdateFields_1_12_1.h:47–100](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/UpdateFields_1_12_1.h) defines the checked
  health, mana, race/class, intellect and fire-resistance words.

| Target level | Miss | Full resist | Crit | Ordinary hit | Partial resist (overlapping) |
| --- | ---: | ---: | ---: | ---: | ---: |
| 60 | 0 | 400/10001 | (9601*258759599)/(10001*4294967296) | (9601*4036207697)/(10001*4294967296) | 0 |
| 63 | 0 | 1700/10001 | (8301*258759599)/(10001*4294967296) | (8301*4036207697)/(10001*4294967296) | (8301*19)/(10001*100) |

Partial resistance overlaps ordinary hits and crits. Only the first four columns sum to one.
The TOML stores exact rational numerators and denominators. Expectations never come from the
small recording or from LyraCore calculations.

## Measurement contract

The submission allocation is now `(7 white-melee categories + 5 spell checks) * 2 levels * 3 seeds
= 72` exact equal-tail binomial tests. Reject at `p <= 1e-6/72`. Bonferroni bounds false failure
at at most 1e-6 without assuming independence between the overlapping categories. The existing
relative-mode recurrence and conservative floating-point allowance are shared by both slices.
The reference recording is diagnostic and outside the submission verdict.

Core uses public seeds `0x4c79726100000001` and `0x4c79726100000002`; main adds the seed from the
first 16 hexadecimal digits of the merge SHA. A new failure calls for revert, without retry.
The spell comparator emits ordinary `DifferenceClass::Rule` signatures: rule `spell_distribution`,
checkpoint by level and seed slot, stream `casts`, opcode `0x0250`, field `outcomes/<check>`.
Keep observed differences in core's baseline and `known_gaps`. A held signature cannot measure
further worsening; report numerical changes separately.

The recorder binds each Character to its own target. A caller starts each trial before sending
`CMSG_CAST_SPELL`. The collector requires one 1500-ms `SMSG_SPELL_START`, one `SMSG_SPELL_GO`, and
exactly one damage log for a landed cast. A refused, duplicate, missing or ineligible outcome is a
setup failure. Observer copies do not add samples. No server histogram supplies evidence.

Run `reference.sh record-checked scenarios/spell_direct_damage_outcomes.toml OUTPUT` under
`newgrp docker`, with `WIRE_BIN` naming the built binary. The wrapper holds the shared audit lock
and takes one completed recording, not two. Sixteen Characters, eight per level, supply 250 casts
each: 2,000 per level. An unretained readiness cast per Character checks setup. Only per-Character
counts, five 50-cast blocks, elapsed/setup time, classifier version, existing binary/source/database
provenance and one cast-start witness enter the scrubbed Golden Trace. No large packet stream is
retained. Cleanup removes only these disposable targets and Characters.

`cargo test --test spell_rolls` covers exact rational tails, zero/one probabilities, allocation,
wire classification, overlapping partials, observer copies, missing/duplicate/changed-target
packets, ineligible contexts, truncated packets and two-point comparator shifts. The controls
are synthetic, not measured gameplay. Core's later PR must add delivered-packet sampling,
repeatability and gameplay-rate sensitivity through the existing fixture-only random Seam.
The generic TCP comparator reports `spell_sample` until it has a full candidate collector; a
copied reference histogram or short packet witness cannot claim statistical parity.

## Single replacement exception, 2026-11-05

The first completed diagnostic passed, but an edit to the running wrapper caused a later shell
read failure. Failure cleanup deleted its accepted Golden Trace. It was not rejected on results.
The retained aggregate counts, ordered miss/resist/crit/hit/partial resist, are
`[0,87,124,1789,0]` at level 60 and `[0,316,97,1587,314]` at level 63. Setup took 473,572 ms;
capture took 463,490 ms. The deleted trace is not reconstructed from these counts.

The maintainer authorized exactly one replacement of 2,000 casts per level. This is a single
exception to ADR 0007's "once, without a repeat recording" rule. Keep context, expected rates,
public seeds and measurement code fixed, and edit no script during capture. Copy the accepted
trace before wrapper cleanup. Use the replacement regardless of its results, report both samples,
and stop if it fails or is lost. No third capture is authorized.

The only replacement completed with setup 464,310 ms and capture 454,361 ms. Its counts are
`[0,77,114,1809,0]` at level 60 and `[0,357,100,1543,324]` at level 63. The diagnostic accepted both
levels. This replacement is the committed Golden Trace, with SHA-256
`958c3f88f01d229477a7376bad28881fdba5c3d31e3bcd62f68d62533d97239f`.
All 59 frozen source/input files matched their pre-capture hashes after the wrapper exited zero.
The accepted bytes were copied and synced before wrapper cleanup, and the two copies matched.
No third recording was taken.
