#!/usr/bin/env bash
# Playerbots GOAL SYSTEM cut-1 acceptance (work-item 149) — fully autonomous, ZERO wire sessions:
#   1. QUEST GOAL: a lone bot near a questgiver + wolves autonomously CHOOSES quest 50900, walks
#      to the giver, accepts, hunts + kills both objective wolves (AGGRO INITIATION — it starts
#      the fights), LOOTS the corpses (money above the quest reward proves it), walks back and
#      turns in (rewarded row + XP).
#   2. GRIND GOAL: with fresh wolves up, the next selection picks GRIND 51000; kills advance
#      `progress` via the on_kill hook.
#   3. DEATH RECOVERY: killed mid-grind, the bot releases (repop -> GHOST), spirit-resses
#      (res sickness and all), and RESUMES the still-active goal — the pad sits inside the goal
#      fence of the Northshire graveyard so the wolves are still in its area after the res.
#   4. HISTORY: finished goal rows are kept — the decision trail is sql-visible.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight bot-goals

WOLF=51000; WOLF_ELDER=51002; GIVER=51003; QUEST=50900
# ~64yd from the Northshire graveyard (-8935,-188): a spirit-res drops the bot back INSIDE the
# 150yd goal fence, so a mid-grind death resumes instead of re-fencing.
PAD_X=-8930.0; PAD_Y=-250.0; PAD_Z=80.0

# ---- staging (no faction rows: the bot INITIATES; wolves retaliate) ----
scall debug_seed_scenario_fixtures || true
scall playerbots_despawn_all || true
purge_entry_rows $WOLF; purge_entry_rows $WOLF_ELDER; purge_entry_rows $GIVER
sqlq "DELETE FROM game_melee_attack" >/dev/null
scall playerbots_spawn_role 1 $PAD_X $PAD_Y $PAD_Z 2 || { echo "[orch] bot spawn failed" >&2; exit 1; }
BOT=$(char_guid Dpsbot1)
[ -z "$BOT" ] && { echo "[orch] no bot guid" >&2; exit 1; }
echo "[orch] bot-goals: bot=$BOT"
# 266: level the bot to 10 BEFORE the baselines. This also cures the imported-node ambient-aggro
# root cause (an L1 bot vs the L1 quest wolves proximity-aggroed at spawn and pre-empted the goal
# walk — "accept: no quest row"); at L10 those L1 wolves are grey (radius 0) so the bot INITIATES
# per its goal. The quest phase keeps the L1 51000 wolves (KILL objective is level-independent;
# reward_xp=90 is granted flat). The GRIND phase below needs ELDERS (an L10 bot's ±3 grind band
# excludes L1 wolves).
scall debug_set_level "$BOT" 10
scall debug_spawn_at_feet "$BOT" $GIVER 8
scall debug_spawn_at_feet "$BOT" $WOLF 15
scall debug_spawn_at_feet "$BOT" $WOLF 20
XP0=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $BOT"); XP0=${XP0:-0}
M0=$(sql1 "SELECT money FROM game_world_entity WHERE guid = $BOT"); M0=${M0:-0}

# ---- 1. the autonomous quest loop ----
wait_for_sql_ge 45 "SELECT COUNT(*) AS n FROM pkg_playerbots_goal WHERE kind = 1" 1 \
  && step_ok "selection: bot chose QUEST $QUEST (goal row)" || step_fail "selection: no QUEST goal within 45s"
wait_for_sql_ge 30 "SELECT COUNT(*) AS n FROM game_character_quest WHERE character_guid = $BOT" 1 \
  && step_ok "accept: quest row landed in the bot's log (walked to the giver)" || step_fail "accept: no quest row within 30s"
DONE=0
for i in $(seq 1 90); do
  R=$(sqlq "SELECT rewarded FROM game_character_quest WHERE character_guid = $BOT" | grep -c true)
  [ "${R:-0}" -ge 1 ] && DONE=1 && break
  sleep 1
done
[ "$DONE" = 1 ] && step_ok "turn-in: quest rewarded (killed both wolves, walked back)" || step_fail "turn-in: never rewarded within 90s"
XP1=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $BOT")
M1=$(sql1 "SELECT money FROM game_world_entity WHERE guid = $BOT")
assert_ge "xp: kill + quest reward XP granted" "$(( ${XP1:-0} - XP0 ))" 90
assert_gt "loot: money above the 150 quest reward (corpse purses looted)" "$(( ${M1:-0} - M0 ))" 150
assert_ge "history: the finished QUEST goal row is kept (state done)" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_goal WHERE kind = 1 AND state = 1")" 1

# ---- 2+3. grind + death recovery + resume ----
# 266: ELDER wolves (51002, L8) for the grind — an L10 bot's GRIND ±3 band excludes the L1 51000,
# and the offsets sit OUTSIDE the elder's 20yd aggro radius so the bot INITIATES (not proximity-
# aggroed). Four (the 3 GRIND_KILLS + 1 slack for a pre-selection uncredited kill).
scall debug_spawn_at_feet "$BOT" $WOLF_ELDER 25
scall debug_spawn_at_feet "$BOT" $WOLF_ELDER 28
scall debug_spawn_at_feet "$BOT" $WOLF_ELDER 31
scall debug_spawn_at_feet "$BOT" $WOLF_ELDER 34
# 90s not 45 (266): a post-quest REST goal (fight damage) can occupy a full selection cycle
# before GRIND gets picked — the known suite-load timing family (270).
wait_for_sql_ge 90 "SELECT COUNT(*) AS n FROM pkg_playerbots_goal WHERE kind = 0 AND state = 0" 1 \
  && step_ok "selection: next goal is GRIND $WOLF_ELDER" || step_fail "selection: no GRIND goal within 90s"
# 90s, matching the selection wait above it and the post-death resume below. 45s covered the KILL
# but not the work in front of it: the bot has to finish selecting the goal, walk the 25-34yd out to
# an elder that was deliberately staged outside its aggro radius, and win an L8 fight — and the two
# assertions on either side of this one already budget 90s for exactly that reasoning (266/270).
wait_for_sql_ge 90 "SELECT progress FROM pkg_playerbots_goal WHERE kind = 0 AND state = 0" 1 \
  && step_ok "grind: a kill advanced progress (on_kill credit)" || step_fail "grind: no kill credit within 90s"

scall debug_set_health "$BOT" 0
D0=$(sql1 "SELECT dead FROM game_world_entity WHERE guid = $BOT" | head -1)
DEADNOW=$(sqlq "SELECT dead FROM game_world_entity WHERE guid = $BOT" | grep -c true)
assert_eq "death: the bot is down mid-goal" "${DEADNOW:-0}" "1"
REVIVED=0
for i in $(seq 1 15); do
  ALIVE=$(sqlq "SELECT dead FROM game_world_entity WHERE guid = $BOT" | grep -c false)
  [ "${ALIVE:-0}" -ge 1 ] && REVIVED=1 && break
  sleep 1
done
[ "$REVIVED" = 1 ] && step_ok "recovery: released + spirit-ressed (alive again, sql-observed)" || step_fail "recovery: still dead after 15s"
STILL=$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_goal WHERE kind = 0 AND state = 0")
assert_ge "recovery: the GRIND goal survived the death (resumes)" "${STILL:-0}" 1
# 180s: this step is the most expensive assertion in the file and had the SAME budget as the single
# kill above it — it needs a corpse run plus THREE real L8 fights (the bot flees at 15% and re-engages,
# so a fight is several approach/flee cycles). Observed completing in ~100-140s and failing at 90 in
# about half of the runs; the window was the flake, not the behaviour.
wait_for_sql_ge 180 "SELECT COUNT(*) AS n FROM pkg_playerbots_goal WHERE kind = 0 AND state = 1" 1 \
  && step_ok "resume: the grind completed after the death (3 kills, state done)" || step_fail "resume: grind never completed within 180s"

# ---- 4. the decision trail ----
assert_ge "history: >= 2 finished goal rows kept (the sql-visible mind)" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_goal WHERE state = 1")" 2

# ---- teardown (asserted) ----
scall playerbots_despawn_all || true
purge_entry_rows $WOLF; purge_entry_rows $WOLF_ELDER; purge_entry_rows $GIVER
sqlq "DELETE FROM game_melee_attack" >/dev/null
assert_eq "teardown: zero goal rows (swept with the bot character)" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_goal")" "0"
assert_eq "teardown: zero bot rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_bot")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[bot-goals] PASS"; exit 0; else echo "[bot-goals] FAIL"; exit 1; fi
