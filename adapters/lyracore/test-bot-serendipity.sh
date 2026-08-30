#!/usr/bin/env bash
# SERENDIPITY GROUPING (276 slice 1) — fully headless, ZERO wire sessions:
#   1. TWO autonomous quester bots near the same fixture questgiver both CHOOSE + accept quest
#      50900 on their own. NO wolves are up yet, so neither can finish — both sit in the
#      OBJECTIVES phase, which is exactly the window the invite scan runs in.
#   2. One bot notices the other (same quest un-rewarded, ungrouped, in range) and INVITES;
#      the bot target auto-accepts server-side (on_group_invite) — ONE group, both bots,
#      formed without any operator nudge. The human-facing path rides the SAME invite core +
#      the existing SMSG_GROUP_INVITE relay.
#   3. Wolves spawn; GROUP KILL CREDIT fans the same kills into BOTH quest logs (group.rs
#      recipients) and both bots turn in — adventuring together, not just standing together.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"
scenario_preflight bot-serendipity
ensure_playerbots_package bot-serendipity

WOLF=51000; GIVER=51003; QUEST=50900
PAD_X=-8930.0; PAD_Y=-250.0; PAD_Z=80.0

# ---- staging: giver + two L10 dps bots, NO wolves yet ----
scall debug_seed_scenario_fixtures || true
scall playerbots_despawn_all || true
purge_entry_rows $WOLF; purge_entry_rows $GIVER
sqlq "DELETE FROM game_melee_attack" >/dev/null
scall playerbots_spawn_role 1 $PAD_X $PAD_Y $PAD_Z 2 || { echo "[orch] bot A spawn failed" >&2; exit 1; }
scall playerbots_spawn_role 1 -8926.0 $PAD_Y $PAD_Z 2 || { echo "[orch] bot B spawn failed" >&2; exit 1; }
A=$(char_guid Dpsbot1); B=$(char_guid Dpsbot2)
{ [ -z "$A" ] || [ -z "$B" ]; } && { echo "[orch] bot guids missing" >&2; exit 1; }
echo "[orch] serendipity: A=$A B=$B"
scall debug_set_level "$A" 10
scall debug_set_level "$B" 10
scall debug_spawn_at_feet "$A" $GIVER 8

# ---- 1. both bots pick the quest up autonomously ----
wait_for_sql_ge 120 "SELECT COUNT(*) AS n FROM game_character_quest WHERE quest_entry = $QUEST" 2 \
  && step_ok "quest: both bots accepted $QUEST autonomously" || step_fail "quest: both never accepted within 120s"

# ---- 2. the serendipity moment: invite + auto-accept, one group with both ----
wait_for_sql_ge 90 "SELECT COUNT(*) AS n FROM game_group_member" 2 \
  && step_ok "serendipity: a group formed (same-quest invite + auto-accept)" || step_fail "serendipity: no group within 90s"
GID_A=$(sql1 "SELECT group_id FROM game_group_member WHERE character_guid = $A")
GID_B=$(sql1 "SELECT group_id FROM game_group_member WHERE character_guid = $B")
[ -n "$GID_A" ] && [ "$GID_A" = "$GID_B" ] \
  && step_ok "serendipity: A and B share one group ($GID_A)" || step_fail "serendipity: A ($GID_A) / B ($GID_B) not one group"

# ---- 3. adventure together: shared kill credit completes BOTH quests off the same wolves ----
scall debug_spawn_at_feet "$A" $WOLF 12
scall debug_spawn_at_feet "$A" $WOLF 16
DONE=0
for i in $(seq 1 120); do
  R=$(sqlq "SELECT rewarded FROM game_character_quest WHERE quest_entry = $QUEST" | grep -c true)
  [ "${R:-0}" -ge 2 ] && DONE=1 && break
  sleep 1
done
[ "$DONE" = 1 ] && step_ok "group play: the SAME two kills credited both members (both rewarded)" \
  || step_fail "group play: both quests never completed within 120s"

# ---- teardown ----
scall playerbots_despawn_all || true
purge_entry_rows $WOLF; purge_entry_rows $GIVER
sqlq "DELETE FROM game_melee_attack" >/dev/null
assert_eq "teardown: zero member rows (group dissolved with the bots)" "$(sql1 "SELECT COUNT(*) AS n FROM game_group_member")" "0"
assert_eq "teardown: zero bot rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_bot")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[bot-serendipity] PASS"; exit 0; else echo "[bot-serendipity] FAIL"; exit 1; fi
