#!/usr/bin/env bash
# EXPLORATION XP (200): entering a fresh subzone awards discovery XP exactly once. Drives the
# movement-hook core (check_area_exploration) via debug_explore_at — which sets the position AND runs
# the check atomically in one reducer, so a stay-session movement heartbeat can't race it — then asserts
# the store row, the discovery-XP grant, and dedup (a second visit into the same area awards nothing).
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight exploration

# Resolve by NAME, never a hardcoded guid (see test-rest-state.sh's note): a stale hardcode reads
# every assertion against a character that does not exist, and reports `got ''` rather than failing.
GINGER=$(char_guid Ginger)
[ -n "$GINGER" ] || { echo "[exploration] no Ginger character on lyracore" >&2; exit 1; }
GOLDSHIRE_X=-9461; GOLDSHIRE_Y=47 # Goldshire subzone (area_bit 548, exploration_level 5 → 70 XP at L5)
FAILED=0
chk(){ if [ "$2" = "$3" ]; then echo "  OK   $1 ($2)"; else echo "  FAIL $1: got '$2' want '$3'"; FAILED=1; fi }
chk_gt(){ # chk_gt label value floor
  if [ "${2:-x}" -gt "$3" ] 2>/dev/null; then echo "  OK   $1 ($2 > $3)"; else echo "  FAIL $1: got '$2' not > $3"; FAILED=1; fi
}

sqlq "DELETE FROM game_character_explored WHERE character_guid = $GINGER" >/dev/null # fresh slate
scall debug_set_level "$GINGER" 5
stay_start TEST Ginger || exit 1

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
FOGOUT=$(timeout 35 "$WC" TEST Ginger values-watch "$GINGER" 1128 20 2>&1)
if grep -q 'VALUES-WATCH PASS' <<<"$FOGOUT" && grep -q 'field 1128 = 16' <<<"$FOGOUT"; then
  echo "  OK   fog restore: PLAYER_EXPLORED_ZONES field 1128 = 16 (Goldshire bit relayed on login)"
else
  echo "  FAIL fog restore: field 1128=16 not relayed on login"; echo "$FOGOUT" | tail -3; FAILED=1
fi

# WIRE ("Discovered" popup): SMSG_EXPLORATION_EXPERIENCE (0x01F8 = 504) is the one-shot "Discovered: <area>"
# text. Unlike the idempotent fog VALUES it must fire ONLY on a LIVE discovery, never replay on login (else
# every already-explored area re-pops) — the gateway's initial-skip set (on_explored_insert). Body =
# area_id u32 LE + experience u32 LE, so Goldshire = (87, 70) at L5.
# NEGATIVE (row still present from the fog check): a fresh login must NOT replay the popup.
if timeout 20 "$WC" TEST Ginger opcode-watch 504 8 2>&1 | grep -q 'OPCODE-WATCH PASS'; then
  echo "  FAIL Discovered popup replayed on login (initial-skip broken)"; FAILED=1
else
  echo "  OK   Discovered popup skipped on login (initial-skip holds)"
fi
# POSITIVE: clear the row, connect+watch, then fire a fresh discovery from outside → popup must arrive.
sqlq "DELETE FROM game_character_explored WHERE character_guid = $GINGER" >/dev/null
POPOUT=$(mktemp)
timeout 30 "$WC" TEST Ginger opcode-watch 504 20 >"$POPOUT" 2>&1 &
POPID=$!
sleep 5
scall debug_explore_at "$GINGER" 0 "$GOLDSHIRE_X" "$GOLDSHIRE_Y"
wait $POPID
if grep -q 'OPCODE-WATCH PASS' "$POPOUT" && grep -q 'body\[0..8\] = (87, 70)' "$POPOUT"; then
  echo "  OK   Discovered popup on live discovery (area 87, +70 XP)"
else
  echo "  FAIL Discovered popup not relayed on live discovery"; tail -3 "$POPOUT"; FAILED=1
fi
rm -f "$POPOUT"

# teardown
sqlq "DELETE FROM game_character_explored WHERE character_guid = $GINGER" >/dev/null
scall debug_set_level "$GINGER" 2
if [ "$FAILED" -eq 0 ]; then echo "[exploration] PASS"; exit 0; else echo "[exploration] FAIL"; exit 1; fi
