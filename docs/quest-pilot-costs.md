# Quest pilot cost log — 2026-11-04

This pilot used harness main `53b34bdd558aae99d5e4e9da8495b9f035b5e4cc` and the existing pinned
Reference Server runtime. LyraCore remained read-only. One author interleaved this work with the
taxi recording repair; elapsed authoring intervals overlap and must not be added together.
No full suite or whole-catalogue strictness run was performed locally.

## Per-scenario costs

| Scenario | Authoring wall time | New Fixture Verbs | Checked recorder attempts | Samples | Stable pair | Strictness |
| --- | --- | --- | --- | --- | --- | --- |
| `quest_offers` | Approximately 15 minutes, 06:11–06:26 UTC, including shared codec and review work | None | 2 | 3 | Yes, 52 and 59 packets | 410 mutations; 390 detected; 20 explained; zero unexplained; 0.98 seconds in release |
| `quest_log` | Approximately 22 minutes, 06:04–06:26 UTC, including both adapters and snapshot verification | `quest(entry, state = "incomplete")` | 1 | 2 | Yes, 53 and 53 packets | 184 mutations; 165 detected; 19 explained; zero unexplained; 0.71 seconds in release |

Authoring intervals are estimates from the work session, not a time-tracking instrument. Recorder
and strictness durations below are measured wall time. Both successful pairs returned no differences.

`quest_offers` attempt 1 took 14.66 seconds and failed its first sample. The scenario and giver
binding used decimal 388 instead of 392 for `0x0188`; the decoder already used the correct hex
opcode. The live refusal exposed this author error. Attempt 2 took 28.99 seconds and passed both
samples after correcting those constants. No Equivalence Rule changed for the retry.

`quest_log` passed its first checked recording in 31.53 seconds. Its invalid swap produced no
Character update. The swap-back and both abandon checkpoints asserted the resulting log entries.

## Rules and verification

Both scenarios use a Character object scope to retain every Character field and omit unrelated
creature updates. Each checkpoint permits only opcode 221 with a decoded creature subject
(`subject_guid_high = 61744`). Each uses a 100 ms default timing bound. No quest field, reward,
requirement, giver identity, reply count or reply order is excluded. No allowlist entry was added.

The new targeted checks cover quest request bytes, quest packet decoding and truncated bodies,
giver identity bound from opening, malformed Fixture Verbs, both adapters' fixture application,
and final login snapshot verification. The snapshot check refuses missing entries, wrong slots,
objective progress, completion and timers. `cargo fmt --all` was applied. Full automated checks
belong to CI; neither a live LyraCore audit nor core tier 1 was run.

Comment review passed for the changed source, tests and adapter code. The only new source comment
states the build-5875 quest-slot layout (20 slots, three words per slot, starting at field 198).
No retained contract, tool directive or documentation pointer was removed. Packet layouts and
fixture requirements are in [quests.md](quests.md).

## What was hard and what remains

Fixture Verbs were not the only missing layer. The scenario model had no quest request types and
the independent decoder had no quest-offer packets. The pilot added both, and bound giver replies
to the opening creature identity rather than accepting any newly allocated GUID.

`vendor_transactions` was inspected first. Its scope includes buy-in-slot, sell, buyback and repair,
not merely buying one item. A useful paid-repair scenario needs damaged equipment. The inspected
core reducers have `debug_repair_item`, but no setter for item damage. Proposed future interface:
`debug_set_item_durability(character_guid: u64, slot: u8, durability: u32)`, validating ownership
and the template maximum. No such reducer was added, and no undamaged-item request is claimed as
repair coverage. Inventory item GUID binding is also needed for sell and repair requests.

`vendor_offers` remains unauthored. Its condition and restock scope needs more than a static vendor
list: the local snapshot includes a 60-second restock item (vendor 9499, item 11325) and reputation
conditions at other vendors. Those cases were inspected but not recorded. Its ledger source URL
at commit `464179081673cfd240f7ffe7f0daf96bca0b5a70` was reachable.

`quest_completion`, `quest_reward` and optional inventory/spell scenarios remain unauthored.
Their quest handler and packet source was inspected, but no coverage is claimed. `item`,
`equipped_item`, `spell` and `reputation` are unneeded by the two completed scenarios and were not
added without a consuming recording. This is a bounded pilot result, not completion of the
scenario-author backlog.

Existing core signatures checked for follow-up work:

- `debug_grant_item(character_guid: u64, entry: u32, count: u32)`
- `debug_equip_item(character_guid: u64, from_slot: u8)` — takes a slot, not an entry.
- `debug_learn_spell(character_guid: u64, spell_id: u32)`
- `debug_grant_quest(character_guid: u64, quest_entry: u32)`
- `debug_accept_quest(character_guid: u64, giver_guid: u64, quest_entry: u32)`
- `debug_grant_reputation(character_guid: u64, faction_id: u32, amount: i32)` — grants a delta;
  the pinned GM `.modify rep` command sets a standing, so simply sharing the numeric argument is wrong.
- `debug_set_skill(character_guid: u64, skill_line: u32, value: u32)`

The exact core integration list for the delivered scenarios is in [quests.md](quests.md#core-integration).
