# Quest pilot evidence

`quest_offers` creates a level-6 human warrior and spawns creature 1977. The Reference Server's
world relates that giver to quest 433: kill ten creatures of entry 1117. The scenario requires
available status 5, the offer from the greeting, a direct details query and the numeric objective
in the quest query response. Querying quest 7 at this unrelated giver must close gossip. Giver
GUIDs must name the creature bound from opening CREATE updates. Text is compared as recorder hashes.

`quest_log` grants quests 7 and 15 to a level-1 human warrior. Both are incomplete kill quests.
The fixture deliberately bypasses their prerequisite chain: this scenario tests log operations,
not acceptance eligibility. Both adapters must verify the final login snapshot before capture:
quests 7 and 15 occupy slots 0 and 1, with no progress, completed flag or timer; other slots are
empty. A failed GM command or reducer therefore fails setup.

The scenario swaps both slots, sends an invalid swap to slot 20, swaps back, then abandons each
quest. The invalid operation must emit no Character update. Swapping back proves that it preserved
both entries. The successful operations assert fields 198 and 201 explicitly; every other Character
field remains compared. No quest field has a tolerance or exclusion.

Both scenarios permit only creature-subject monster movement at each named checkpoint. They
retain packet presence, reply order and field values. The object scope excludes unrelated creatures
while retaining every Character field. Neither scenario permits arbitrary unexpected packets.

`quest_completion` stages incomplete quest 7 followed by completed quest 2158. The latter has
no objectives, so normal acceptance completes it in both adapters; no objective mutation is used.
The final login must contain the entries in slots 0 and 1, progress words 0 and 0x01000000,
and no timers. The adapters refuse any other progress or state flag. Its Stormwind position keeps
the spawned Marshal McBride (197) distinct from his imported Northshire spawn. Innkeeper Farley
(295) offers the completion reward for quest 2158. The scenario compares an incomplete completion
menu, silence on an incomplete reward request, and both complete reward-offer request paths.

## Sources

These sources were read through raw URLs at the Reference Server's pinned commit:

- [Quest handlers](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Handlers/QuestHandler.cpp):
  `HandleQuestgiverStatusQueryOpcode`, `HandleQuestgiverHelloOpcode`,
  `HandleQuestgiverQueryQuestOpcode`, `HandleQuestQueryOpcode`, `HandleQuestLogSwapQuest` and
  `HandleQuestLogRemoveQuest`.
- [Quest packet layouts](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Server/Packets/Quest.cpp):
  full GUIDs, 32-bit entries, byte-sized quest-log slots, quest details and query response fields.
- [Quest definitions](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/QuestDef.h):
  available status 5.
- [Character commands](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Commands/CharacterCommands.cpp):
  `.quest add ENTRY CHARACTER` uses `HandleQuestAddCommand`.
- [Character state](https://raw.githubusercontent.com/vmangos/core/d9173eca3ea08dd985c52dabbcc826898d1d0b77/src/game/Objects/Player.h):
  three words per quest slot, objective counters and state in the second word, timer in the third.

Entries and objective counts were checked in the local world snapshot at database commit
`464179081673cfd240f7ffe7f0daf96bca0b5a70` and then asserted against the Reference Server recordings.
No world rows or client files are included.

## Core integration

At the next harness pin, add `FixtureVerb::Quest { entry, state: QuestState::Incomplete }` to
both tier 1 fixture paths in `module/tests/conformance/transport.rs`. Apply
`debug_grant_quest(character_guid: u64, quest_entry: u32)` in fixture order. Prepare the quest
and objective templates before granting them. Verify their log slots and incomplete state.
No new reducer is required for these scenarios.

Register both scenario ids in their existing Behavior ledger entries and add tier 1 tests.
`quest_offers` needs imported creature 1977, its relation to quest 433 and that quest's requirements
and rewards. `quest_log` needs templates 7 and 15 with their real kill objectives. Advance the
harness revision, dependency revision and lockfile together, then generate and inspect the new
baselines. These recordings make no claim that core currently passes either scenario.

At the pin that adds `quest_completion`, both tier 1 paths must also accept `QuestState::Complete`.
Continue to call `debug_grant_quest`; complete means the ordinary grant effects already satisfy the
quest. Verify 0x01000000 in the quest state word, with zero objective progress and timer. This is
not a facility for setting arbitrary kill or exploration credit. Seed quest 7's ten entry-6 kills,
quest 2158's two item choices (4656 and 159, five each), their templates and the respective end-giver
relations to creatures 197 and 295. Register the scenario and its tier 1 test, then generate its baseline.

World-text fields are data comparisons. Tier 1 authors fixture text locally and seeds only numeric
ids and values from the pinned world. Quest titles, details, objectives and menu text remain hashed
in the Reference Server recording. A text value difference is data; a missing field, changed type,
packet shape, numeric value or giver identity remains a rule difference.
