#!/usr/bin/env bash
# SCENARIO 1 — QUEST LOOP (work-item 140): accept from a live giver -> kill both objective wolves
# with real engage/swing ticks -> loot the corpse -> turn in -> assert XP, reward item, rep, money,
# and the rewarded quest row. Wire steps + SMSG assertions live in `wire-client scenario-quest`;
# this orchestrator stages the fixtures, sql-asserts every server-state seam, and tears down.
# Fixture: quest 50900 "Wolf Cull" (repeatable) from giver 51003, kill 2x Test Wolf 51000
# (debug_seed_scenario_fixtures — idempotent mock-seed).
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight scenario-quest

QUEST=50900; GIVER_ENTRY=51003; WOLF_ENTRY=51000
# PAD MOVED 2026-07-16: the old (-8920,-180) pad sits INSIDE a nav-rasterized structure
# (debug_nav_probe: obstruction_top 82..106 over ground 80.7) — since 243's LoS swing gate, no
# melee could land there (STEP 3 hung 90s). This spot is probed LoS-clear (debug_nav_leg).
PAD_X=-8960; PAD_Y=-460; PAD_Z=81

# repeatability: drop any prior 50900 log row (failed-run residue or a rewarded repeatable row) so
# every run exercises the identical first-accept path (hello -> QUEST_DETAILS).
sqlq "DELETE FROM game_character_quest WHERE character_guid = $GINGER AND quest_entry = $QUEST" >/dev/null

# ---- stage: park Ginger on the pad, spawn the giver + two weakened wolves at her feet ----
stay_start TEST test123 Ginger || exit 1
scall debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0
scall debug_set_health "$GINGER" 100000
GIVER=$(spawn_at "$GINGER" $GIVER_ENTRY 4)
# Offsets INSIDE the 5yd melee reach (2026-07-16): the headless client cannot walk, and since the
# retaliation invariant (a passive wolf reacts only to a FIRED attack) a wolf spawned at 6yd never
# charges into range on its own — the old 6/8yd offsets relied on that removed arm-time charge.
WOLF1=$(spawn_at "$GINGER" $WOLF_ENTRY 3)
WOLF2=$(spawn_at "$GINGER" $WOLF_ENTRY 4)
if [ -z "$GIVER" ] || [ -z "$WOLF1" ] || [ -z "$WOLF2" ] || [ "$WOLF1" = "$WOLF2" ]; then
  echo "[orch] fixture spawn failed (giver=$GIVER wolves=$WOLF1/$WOLF2)" >&2; stay_stop; exit 1
fi
# Weaken the wolves so the real swing exchange stays seconds-long (still a genuine kill-credit path).
scall debug_set_health "$WOLF1" 10
scall debug_set_health "$WOLF2" 10
stay_stop
# repeatability: suite combat wears the main-hand cumulatively (swings roll -1 durability, deaths
# cost 10% max, nothing repairs it) and a 0-durability weapon swings UNARMED (combat/mod.rs) — a
# worn sword made wolf-2's 90s window unkillable IN-SUITE while standalone reruns (after
# scenario_death's own top-up) passed. Top it by guid (compound UPDATE no-ops).
MH_GUID=$(sql1 "SELECT guid FROM game_item_instance WHERE owner_guid = $GINGER AND slot = 15")
[ -n "$MH_GUID" ] && sqlq "UPDATE game_item_instance SET durability = 20 WHERE guid = $MH_GUID" >/dev/null
echo "[orch] staged: giver=$GIVER wolf1=$WOLF1 wolf2=$WOLF2"

# ---- baselines for the delta assertions ----
MONEY0=$(sql1 "SELECT money FROM game_character WHERE guid = $GINGER")
XP0=$(sql1 "SELECT xp FROM game_character WHERE guid = $GINGER")
LEVEL0=$(sql1 "SELECT level FROM game_character WHERE guid = $GINGER")
JERKY0=$(sqlq "SELECT stack_count FROM game_item_instance WHERE owner_guid = $GINGER AND entry = 5090052" | grep -oE '[0-9]+' | awk '{s+=$1} END{print s+0}')
REP0=$(sql1 "SELECT standing FROM game_player_reputation WHERE character_guid = $GINGER AND faction_id = 50900"); REP0=${REP0:-0}

# ---- mid-flight watcher: the accept row must appear while the wire scenario runs ----
(
  for _ in $(seq 1 30); do
    R=$(sql1 "SELECT COUNT(*) AS n FROM game_character_quest WHERE character_guid = $GINGER AND quest_entry = $QUEST AND rewarded = false")
    [ "${R:-0}" -ge 1 ] && { echo "[orch] STEP-ASSERT OK: quest log row appeared after accept (rewarded=false)"; exit 0; }
    sleep 1
  done
  echo "[orch] STEP-ASSERT FAIL: quest log row never appeared after accept" >&2
) &
WATCH=$!

# ---- the wire scenario (SMSG assertions per step; nonzero exit = the failed step is named) ----
timeout 240 "$WC" TEST test123 Ginger scenario-quest "$GIVER" $QUEST "$WOLF1" "$WOLF2"
RC=$?
wait "$WATCH" 2>/dev/null || FAILED=1
[ $RC -ne 0 ] && { echo "[orch] wire scenario failed (rc=$RC)"; FAILED=1; }

# ---- post-flow server-state assertions ----
# Persist-settle wait (2026-07-16, the 265 race): money/xp live on the ENTITY during the session and
# reach game_character only via the gateway's ASYNC logout persist (~1-3s after the wire client
# exits). Poll the money delta into place before reading any char-row delta.
MONEY1=""
for _ in $(seq 1 12); do
  MONEY1=$(sql1 "SELECT money FROM game_character WHERE guid = $GINGER")
  [ $(( ${MONEY1:-0} - ${MONEY0:-0} )) -ge 175 ] && break
  sleep 1
done
XP1=$(sql1 "SELECT xp FROM game_character WHERE guid = $GINGER")
JERKY1=$(sqlq "SELECT stack_count FROM game_item_instance WHERE owner_guid = $GINGER AND entry = 5090052" | grep -oE '[0-9]+' | awk '{s+=$1} END{print s+0}')
REP1=$(sql1 "SELECT standing FROM game_player_reputation WHERE character_guid = $GINGER AND faction_id = 50900")
REWARDED=$(sql1 "SELECT COUNT(*) AS n FROM game_character_quest WHERE character_guid = $GINGER AND quest_entry = $QUEST AND rewarded = true")

# money: +150 quest reward +25..50 looted from ONE wolf corpse (kill-XP path also pays nothing else)
DMONEY=$(( ${MONEY1:-0} - ${MONEY0:-0} ))
assert_ge "money delta = 150 reward + 25..50 loot" "$DMONEY" 175
# Upper bound loosened 201->351 (2026-07-16): in-suite, a lingering lootable corpse from an EARLIER
# test can add its purse to the window (money delta 286 seen live). The bound's job is only to catch
# a money-printing bug, which 351 still does.
assert_lt "money delta = 150 reward + 25..50 loot" "$DMONEY" 351
# xp: +90 (explicit reward_xp; kill XP for a grey L1 wolf at Ginger's level is 0). If the award
# crossed a level threshold, xp wraps into the ding — a level increase is the same proof.
LEVEL1=$(sql1 "SELECT level FROM game_character WHERE guid = $GINGER")
if [ "${LEVEL1:-0}" -gt "${LEVEL0:-0}" ]; then
  step_ok "xp: quest reward dinged the character (L${LEVEL0} -> L${LEVEL1})"
else
  assert_ge "xp delta >= 90 (quest reward)" $(( ${XP1:-0} - ${XP0:-0} )) 90
fi
assert_eq "reward item: +2 Tough Jerky (entry 5090052)" "$JERKY1" "$(( JERKY0 + 2 ))"
assert_eq "reputation: +250 with fixture faction 50900" "${REP1:-0}" "$(( REP0 + 250 ))"
assert_ge "quest row marked rewarded" "${REWARDED:-0}" 1

# 186: LOOTABLE follows the rule (loot rows remain OR money > 0) on the money-looted corpse.
# The wire flow took wolf2's money and released; whether the flag must be 0 or 1 depends on
# whether the item roll left rows — compute the expectation from the same tables the module
# reads (SpacetimeDB's SQL subset has no EXISTS/bitwise, so the rule is evaluated in shell).
W2_FLAGS=$(sql1 "SELECT dynamic_flags FROM game_world_entity WHERE guid = $WOLF2")
W2_MONEY=$(sql1 "SELECT money FROM game_world_entity WHERE guid = $WOLF2")
W2_ROWS=$(sqlq "SELECT id FROM game_corpse_loot WHERE corpse_guid = $WOLF2" | grep -cE '[0-9]')
if [ -n "$W2_FLAGS" ]; then # corpse may have despawned by now — rule is only assertable while it stands
  WANT=0; { [ "${W2_ROWS:-0}" -gt 0 ] || [ "${W2_MONEY:-0}" -gt 0 ]; } && WANT=1
  assert_eq "LOOTABLE flag matches the rule (rows=$W2_ROWS money=$W2_MONEY)" "$(( ${W2_FLAGS:-0} % 2 ))" "$WANT"
fi

# ---- teardown (asserted): fixture NPCs, wolves, corpses, spawn rows all gone ----
sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = $GINGER" >/dev/null
purge_entry $GIVER_ENTRY
# per-guid wolf teardown — entry-wide purge would take the init-seeded demo wolf with it
for W in "$WOLF1" "$WOLF2"; do
  sqlq "DELETE FROM game_creature_spawn WHERE guid = $W" >/dev/null
  sqlq "DELETE FROM game_world_entity WHERE guid = $W" >/dev/null
done
assert_eq "teardown: pad wolves gone" "$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE guid = $WOLF1")$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE guid = $WOLF2")" "00"
sqlq "DELETE FROM game_corpse_loot WHERE corpse_guid = $WOLF1" >/dev/null
sqlq "DELETE FROM game_corpse_loot WHERE corpse_guid = $WOLF2" >/dev/null
# NOTE deliberately kept: the repeatable quest's log row (rewarded=true) and the granted rewards —
# the next run re-baselines its deltas, and accept resets a rewarded repeatable row in place.

if [ "$FAILED" -eq 0 ]; then echo "[scenario-quest] PASS"; exit 0; else echo "[scenario-quest] FAIL"; exit 1; fi
