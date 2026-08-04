#!/usr/bin/env bash
# Group-loot acceptance (work-item 187 done-when) — two real wire sessions, all headless:
#   1. LOOT METHOD: the leader's CMSG_LOOT_METHOD(GroupLoot, Uncommon) round-trips — the module
#      stores it and both members' SMSG_GROUP_LIST re-render carries the setting (wire + SQL).
#   2. NEED-BEFORE-GREED: a GREEN (q2) drop on a group kill opens a roll window on BOTH clients
#      (SMSG_LOOT_START_ROLL); leader votes NEED, member GREED; both receive SMSG_LOOT_ROLL_WON
#      naming the LEADER, and the item lands in the leader's bags (game_item_instance).
#   3. ROUND-ROBIN: with the green removed, grey-only kills alternate the corpse's
#      designated_looter_guid between the two members kill-by-kill.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight group-loot
DFS=$(char_guid dfsdfsd)
[ -z "$DFS" ] && timeout 60 "$WC" TEST2 dfsdfsd logout >/dev/null 2>&1 && DFS=$(char_guid dfsdfsd)
[ -z "$DFS" ] && { echo "[orch] no dfsdfsd character" >&2; exit 1; }

WOLF_ENTRY=51000; GREEN=1116; GREY=52  # 1116 Ring of Pure Silver: a REAL imported q2 green (the seeded 50 imports at q1 on a dump-loaded node)
PAD_X=-8905.0; PAD_Y=-440.0; PAD_Z=82.0

# repeatability: clean group state, co-locate on the pad, seed the drops (green 100% + grey 100%)
sqlq "DELETE FROM game_group" >/dev/null
sqlq "DELETE FROM game_group_member WHERE character_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_group_member WHERE character_guid = $DFS" >/dev/null
sqlq "DELETE FROM game_group_invite WHERE target_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_group_invite WHERE target_guid = $DFS" >/dev/null
sqlq "DELETE FROM game_loot_roll" >/dev/null
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z WHERE guid = $GINGER" >/dev/null
sqlq "UPDATE game_character SET x = -8902.0, y = -440.0, z = $PAD_Z WHERE guid = $DFS" >/dev/null
sqlq "DELETE FROM game_creature_loot WHERE creature_entry = $WOLF_ENTRY" >/dev/null
# EXPLICIT reserved ids: the ETL imports creature_loot with explicit ids, so an id-0 SQL insert
# collides with the stale sequence and silently no-ops (the same errno-12 class as the fixture seed).
sqlq "DELETE FROM game_creature_loot WHERE id = 5091000" >/dev/null
sqlq "DELETE FROM game_creature_loot WHERE id = 5091001" >/dev/null
sqlq "INSERT INTO game_creature_loot (id, creature_entry, item_entry, chance_bp, count, group_id, quest_only) VALUES (5091000, $WOLF_ENTRY, $GREEN, 10000, 1, 0, false)" >/dev/null
sqlq "INSERT INTO game_creature_loot (id, creature_entry, item_entry, chance_bp, count, group_id, quest_only) VALUES (5091001, $WOLF_ENTRY, $GREY, 10000, 1, 0, false)" >/dev/null
assert_eq "seed: green+grey loot rows present" "$(sql1 "SELECT COUNT(*) AS n FROM game_creature_loot WHERE creature_entry = $WOLF_ENTRY")" "2"

HOLD_L=/tmp/ws_loot_leader_$$; HOLD_V=/tmp/ws_loot_voter_$$
rm -f "$HOLD_L" "$HOLD_V" "$HOLD_L".* "$HOLD_V".* 2>/dev/null

# ---- form the party; leader sets GroupLoot/Uncommon (clause 1 asserted wire-side in-mode) ----
timeout 400 "$WC" TEST2 dfsdfsd loot-voter greed "$HOLD_V" >/tmp/ws_loot_voter.log 2>&1 &
VOTER=$!
sleep 3
timeout 400 "$WC" TEST Ginger loot-leader dfsdfsd need "$HOLD_L" >/tmp/ws_loot_leader.log 2>&1 &
LEADER=$!
for _ in $(seq 1 40); do [ -f "$HOLD_L.method" ] && break; sleep 1; done
[ -f "$HOLD_L.method" ] && step_ok "wire: loot method set + GROUP_LIST echo (GroupLoot/Uncommon)" \
  || { step_fail "wire: loot-method echo never arrived"; tail -3 /tmp/ws_loot_leader.log; }
assert_eq "sql: group loot_method stored" "$(sql1 "SELECT loot_method FROM game_group")" "3"  # 3 = GROUP (module group::loot_method)

# ---- green kill -> roll window -> NEED beats GREED (clause 2) ----
BAG0=$(sql1 "SELECT COUNT(*) AS n FROM game_item_instance WHERE owner_guid = $GINGER AND entry = $GREEN")
WOLF=$(spawn_at "$GINGER" $WOLF_ENTRY 3)
[ -z "$WOLF" ] && step_fail "wolf spawn failed"
scall debug_kill_creature "$GINGER" "${WOLF:-0}" || step_fail "kill reducer failed"
for _ in $(seq 1 30); do [ -f "$HOLD_L.won" ] && [ -f "$HOLD_V.won" ] && break; sleep 1; done
if [ -f "$HOLD_L.won" ] && [ -f "$HOLD_V.won" ]; then
  step_ok "wire: both members saw the roll window + SMSG_LOOT_ROLL_WON"
else
  step_fail "wire: roll cycle incomplete (leader=$([ -f $HOLD_L.won ] && echo ok || echo no) voter=$([ -f $HOLD_V.won ] && echo ok || echo no))"
  tail -n 4 /tmp/ws_loot_leader.log /tmp/ws_loot_voter.log
fi
WON_L=$(cat "$HOLD_L.won" 2>/dev/null); WON_V=$(cat "$HOLD_V.won" 2>/dev/null)
assert_eq "wire: both sides agree on the outcome" "$WON_L" "$WON_V"
assert_eq "wire: NEED (leader) beat GREED" "${WON_L%% *}" "$GINGER"
assert_eq "wire: the green was the rolled item" "${WON_L##* }" "$GREEN"
sleep 1
BAG1=$(sql1 "SELECT COUNT(*) AS n FROM game_item_instance WHERE owner_guid = $GINGER AND entry = $GREEN")
assert_eq "sql: winner received the item" "$(( ${BAG1:-0} - ${BAG0:-0} ))" "1"

# ---- round-robin on grey-only kills (clause 3): remove the green, two more kills ----
sqlq "DELETE FROM game_creature_loot WHERE creature_entry = $WOLF_ENTRY AND item_entry = $GREEN" >/dev/null
WOLF2=$(spawn_at "$GINGER" $WOLF_ENTRY 4)
scall debug_kill_creature "$GINGER" "${WOLF2:-0}" || step_fail "grey kill 1 failed"
sleep 1
RR1=$(sql1 "SELECT designated_looter_guid FROM game_corpse_loot WHERE corpse_guid = ${WOLF2:-0}")
WOLF3=$(spawn_at "$GINGER" $WOLF_ENTRY 5)
scall debug_kill_creature "$GINGER" "${WOLF3:-0}" || step_fail "grey kill 2 failed"
sleep 1
RR2=$(sql1 "SELECT designated_looter_guid FROM game_corpse_loot WHERE corpse_guid = ${WOLF3:-0}")
if [ -n "$RR1" ] && [ -n "$RR2" ] && [ "$RR1" != "$RR2" ]; then
  step_ok "sql: round-robin alternated the designated looter ($RR1 -> $RR2)"
else
  step_fail "sql: round-robin did not alternate (kill1=$RR1 kill2=$RR2)"
fi

# ---- release + teardown ----
touch "$HOLD_L"; touch "$HOLD_V"
wait "$LEADER"; RC_L=$?
wait "$VOTER"; RC_V=$?
[ $RC_L -eq 0 ] && step_ok "wire: loot-leader flow green" || { step_fail "loot-leader rc=$RC_L"; tail -3 /tmp/ws_loot_leader.log; }
[ $RC_V -eq 0 ] && step_ok "wire: loot-voter flow green" || { step_fail "loot-voter rc=$RC_V"; tail -3 /tmp/ws_loot_voter.log; }
for W in "${WOLF:-}" "${WOLF2:-}" "${WOLF3:-}"; do
  [ -n "$W" ] || continue
  sqlq "DELETE FROM game_creature_spawn WHERE guid = $W" >/dev/null
  sqlq "DELETE FROM game_world_entity WHERE guid = $W" >/dev/null
  sqlq "DELETE FROM game_corpse_loot WHERE corpse_guid = $W" >/dev/null
done
sqlq "DELETE FROM game_creature_loot WHERE creature_entry = $WOLF_ENTRY" >/dev/null
sqlq "DELETE FROM game_item_instance WHERE owner_guid = $GINGER AND entry = $GREEN" >/dev/null
sqlq "DELETE FROM game_loot_roll" >/dev/null
rm -f "$HOLD_L" "$HOLD_V" "$HOLD_L".* "$HOLD_V".* 2>/dev/null
assert_eq "teardown: zero group rows" "$(sql1 "SELECT COUNT(*) AS n FROM game_group")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[group-loot] PASS"; exit 0; else echo "[group-loot] FAIL"; exit 1; fi
