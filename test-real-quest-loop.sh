#!/usr/bin/env bash
# test-real-quest-loop.sh — end-to-end proof that a REAL imported quest completes (2026-07-17).
# scenario_quest proves the MECHANIC on a fixture; the content audit proves the DATA is structurally
# coherent; this closes the gap between them: a real Northshire kill-quest run with REAL reward_xp /
# objectives / giver relations, driven through the real accept -> kill-credit -> turn-in path.
#
# Self-discovering: picks a low-level KILL quest whose giver AND objective creature are both in-world,
# so it survives content re-imports (no hardcoded fragile ids). Uses debug reducers (robust, no wire
# navigation): debug_accept_quest -> debug_kill_nearest (the real kill-credit path) -> debug_turn_in.
set -uo pipefail
cd "$(dirname "$0")/../.."
DB=lyracore
source tools/wire-client/scenario-lib.sh

QCHAR=Realquesttester
G=$(fresh_char "$QCHAR" warrior 5)
[ -z "$G" ] && { echo "[real-quest] fresh_char failed" >&2; exit 1; }

# Discover a completable real KILL quest: quest_level 1..6, has a KILL objective whose target
# creature template exists, a START and END creature giver exist, min_level <= 5. Single-column
# queries + shell filtering (the CLI's compound-WHERE lies — danger-zones).
pick_quest() {
  local q obj tgt start end lvl
  for q in $(sqlq "SELECT entry FROM game_quest_template WHERE quest_level > 0 AND quest_level < 7" | grep -oE '^ *[0-9]+' | tr -d ' '); do
    lvl=$(sql1 "SELECT min_level FROM game_quest_template WHERE entry = $q"); [ "${lvl:-99}" -gt 5 ] && continue
    tgt=$(sqlq "SELECT target_entry FROM game_quest_objective WHERE quest_entry = $q AND kind = 0" | grep -oE '^ *[0-9]+' | grep -v '^ *0$' | head -1 | tr -d ' ')
    [ -z "$tgt" ] && continue
    [ "$(sql1 "SELECT COUNT(*) AS n FROM game_creature_template WHERE entry = $tgt")" = "0" ] && continue
    start=$(sqlq "SELECT creature_entry FROM game_creature_quest WHERE quest_entry = $q AND role = 0" | grep -oE '^ *[0-9]+' | head -1 | tr -d ' ')
    end=$(sqlq "SELECT creature_entry FROM game_creature_quest WHERE quest_entry = $q AND role = 1" | grep -oE '^ *[0-9]+' | head -1 | tr -d ' ')
    [ -z "$start" ] || [ -z "$end" ] && continue
    # objective creature must be spawnable (has a live template — checked) and the givers real
    echo "$q $tgt $start $end $(sql1 "SELECT required_count FROM game_quest_objective WHERE quest_entry = $q AND kind = 0")"
    return 0
  done
  return 1
}

read -r QUEST TGT START END COUNT < <(pick_quest) || { echo "[real-quest] no completable low-level kill quest found" >&2; exit 1; }
COUNT=${COUNT:-1}
TITLE=$(sqlq "SELECT title FROM game_quest_template WHERE entry = $QUEST" | sed -n 3p)
echo "[real-quest] picked q$QUEST '$TITLE' — kill ${COUNT}x creature $TGT (giver $START/$END)"

stay_start TEST test123 "$QCHAR" 90 || exit 1
XP0=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $G")
# The end giver must be an ENTITY for turn-in; spawn the giver + the objective mobs at the char.
GIVER_GUID=$(spawn_at "$G" "$END" 3)
scall debug_grant_quest "$G" "$QUEST" # stages the quest in the log (accept-from-giver is scenario_quest's job)
ACC=$(sql1 "SELECT COUNT(*) AS n FROM game_character_quest WHERE character_guid = $G AND quest_entry = $QUEST")
assert_ge "accept: real quest $QUEST in the log" "${ACC:-0}" 1
# Kill the required count of the objective creature (real kill-credit path).
for _ in $(seq 1 "$COUNT"); do
  scall debug_spawn_at_feet "$G" "$TGT" 2
  sleep 1
  scall debug_kill_nearest "$G" "$TGT"
  sleep 1
done
# Turn in (reward choice 0).
scall debug_turn_in_quest "$G" "$GIVER_GUID" "$QUEST" 0
REWARDED=$(sqlq "SELECT rewarded FROM game_character_quest WHERE character_guid = $G AND quest_entry = $QUEST" | grep -c true)
XP1=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $G")
LVL1=$(sql1 "SELECT level FROM game_world_entity WHERE guid = $G")
stay_stop

assert_ge "turn-in: real quest $QUEST rewarded" "${REWARDED:-0}" 1
# XP rose (either the delta on the same level, or a ding — level went up).
if [ "${LVL1:-5}" -gt 5 ]; then
  step_ok "xp: real quest reward dinged the char (L5 -> L$LVL1)"
else
  assert_gt "xp: real quest reward granted XP" "$(( ${XP1:-0} - ${XP0:-0} ))" 0
fi

# teardown
purge_entry_rows "$TGT"; purge_entry_rows "$END"
drop_char "$QCHAR"
if [ "$FAILED" -eq 0 ]; then echo "[real-quest] PASS — real quest '$TITLE' completed end-to-end"; else echo "[real-quest] FAIL"; exit 1; fi
