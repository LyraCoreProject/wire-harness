#!/usr/bin/env bash
# EXPLORATION XP (200): entering a fresh subzone awards discovery XP exactly once. Drives the
# movement-hook core (check_area_exploration) via debug_explore_at — which sets the position AND runs
# the check atomically in one reducer, so a stay-session movement heartbeat can't race it — then asserts
# the store row, the discovery-XP grant, and dedup (a second visit into the same area awards nothing).
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight exploration

GINGER=9
GOLDSHIRE_X=-9461; GOLDSHIRE_Y=47 # Goldshire subzone (area_bit 548, exploration_level 5 → 70 XP at L5)
FAILED=0
chk(){ if [ "$2" = "$3" ]; then echo "  OK   $1 ($2)"; else echo "  FAIL $1: got '$2' want '$3'"; FAILED=1; fi }
chk_gt(){ # chk_gt label value floor
  if [ "${2:-x}" -gt "$3" ] 2>/dev/null; then echo "  OK   $1 ($2 > $3)"; else echo "  FAIL $1: got '$2' not > $3"; FAILED=1; fi
}

sqlq "DELETE FROM game_character_explored WHERE character_guid = $GINGER" >/dev/null # fresh slate
scall debug_set_level "$GINGER" 5
stay_start TEST test123 Ginger || exit 1

XP0=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $GINGER")
scall debug_explore_at "$GINGER" 0 "$GOLDSHIRE_X" "$GOLDSHIRE_Y"
sleep 1
XP1=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $GINGER")
N1=$(sql1 "SELECT COUNT(*) AS n FROM game_character_explored WHERE character_guid = $GINGER")
BIT=$(sql1 "SELECT area_bit FROM game_character_explored WHERE character_guid = $GINGER")
chk "Goldshire recorded (area_bit 548)" "$BIT" "548"
chk "explored-store row inserted" "$N1" "1"
chk_gt "discovery XP granted" "$((XP1-XP0))" "0"

# dedup: re-enter the same area → no new store row, no more XP
scall debug_explore_at "$GINGER" 0 "$GOLDSHIRE_X" "$GOLDSHIRE_Y"
sleep 1
N2=$(sql1 "SELECT COUNT(*) AS n FROM game_character_explored WHERE character_guid = $GINGER")
XP2=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $GINGER")
chk "dedup: no second store row" "$N2" "1"
chk "dedup: no second XP award" "$((XP2-XP1))" "0"

stay_stop

# WIRE (fog restore): a fresh login relays the PLAYER_EXPLORED_ZONES word for the explored area, so the
# client's map fog is correct on login. Goldshire area_bit 548 → word 17 → field 1111+17=1128, and
# bit 548%32=4 → value 16. (This is also the live fog-clear path — the same on_insert relay.)
FOGOUT=$(timeout 35 "$WC" TEST test123 Ginger values-watch "$GINGER" 1128 20 2>&1)
if grep -q 'VALUES-WATCH PASS' <<<"$FOGOUT" && grep -q 'field 1128 = 16' <<<"$FOGOUT"; then
  echo "  OK   fog restore: PLAYER_EXPLORED_ZONES field 1128 = 16 (Goldshire bit relayed on login)"
else
  echo "  FAIL fog restore: field 1128=16 not relayed on login"; echo "$FOGOUT" | tail -3; FAILED=1
fi

# teardown
sqlq "DELETE FROM game_character_explored WHERE character_guid = $GINGER" >/dev/null
scall debug_set_level "$GINGER" 2
if [ "$FAILED" -eq 0 ]; then echo "[exploration] PASS"; exit 0; else echo "[exploration] FAIL"; exit 1; fi
