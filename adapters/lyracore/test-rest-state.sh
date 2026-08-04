#!/usr/bin/env bash
# REST STATE (196): entering an inn/rest-area fixture sets RESTED — the PLAYER_BYTES_2 rest byte flips
# (zzz icon + blue XP bar), the durable resting flag + live-accrual clock start, and the whole thing
# relays live to a connected client. Drives the movement hook (check_rest_state) via debug_check_rest_at
# (sets position + runs the check atomically, like debug_explore_at) and asserts the store state + the
# PLAYER_BYTES_2 wire relay. The offline rate split + accrual math are unit-tested (module `xp` tests).
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"
scenario_preflight rest-state

# Resolve by NAME, never a hardcoded guid: the fixture's guid changes whenever the character is
# re-created (Ginger has been 3, 9 and 13). A stale hardcode does not fail loudly — every SQL read
# returns NO ROWS, so each assertion reports `got ''` and reads like a product regression.
G=$(char_guid Ginger)
[ -n "$G" ] || { echo "[rest-state] no Ginger character on lyracore" >&2; exit 1; }
INN_X=-9464; INN_Y=42 # Lion's Pride Inn fixture (Goldshire) — REST_TRIGGERS in module/src/rest.rs
FAR_X=-8930; FAR_Y=-160 # open field, outside any rest trigger
FAILED=0
chk(){ if [ "$2" = "$3" ]; then echo "  OK   $1 ($2)"; else echo "  FAIL $1: got '$2' want '$3'"; FAILED=1; fi }
byte3(){ spacetime sql lyracore "SELECT player_bytes_2 FROM game_world_entity WHERE guid = $G" 2>/dev/null | grep -v WARNING | sed -n 3p | awk '{printf "%d", int($1/16777216)%256}'; }
cval(){ spacetime sql lyracore "SELECT $1 FROM game_character WHERE guid = $G" 2>/dev/null | grep -v WARNING | sed -n 3p | tr -d ' '; }

# fresh slate
sqlq "DELETE FROM game_rest_state_event WHERE character_guid = $G" >/dev/null
sqlq "UPDATE game_character SET resting = false, rested_since_micros = 0 WHERE guid = $G" >/dev/null
spacetime call lyracore -- debug_set_health $G 200 >/dev/null 2>&1
stay_start TEST Ginger || exit 1
sleep 1

# ENTER the inn: RESTED byte, resting flag on, accrual clock started.
scall debug_check_rest_at $G 0 "$INN_X" "$INN_Y"
sleep 1
chk "inn: PLAYER_BYTES_2 byte3 = RESTED (0x01)" "$(byte3)" "1"
chk "inn: character.resting" "$(cval resting)" "true"
RS=$(cval rested_since_micros)
if [ "${RS:-0}" -gt 0 ] 2>/dev/null; then echo "  OK   inn: live-accrual clock started ($RS)"; else echo "  FAIL inn: rested_since not started ($RS)"; FAILED=1; fi

# LEAVE to the field: NORMAL byte, resting flag off, clock stopped.
scall debug_check_rest_at $G 0 "$FAR_X" "$FAR_Y"
sleep 1
chk "field: PLAYER_BYTES_2 byte3 = NORMAL (0x02)" "$(byte3)" "2"
chk "field: character.resting off" "$(cval resting)" "false"
chk "field: accrual clock stopped" "$(cval rested_since_micros)" "0"
stay_stop

# WIRE relay: a LIVE inn crossing must push PLAYER_BYTES_2 (field 194) so the client flips zzz/blue-bar
# without a relog. 16777216 = 0x01000000 (byte3 = RESTED, facial-hair byte 0). Starts NOT resting.
sqlq "UPDATE game_character SET resting = false, rested_since_micros = 0 WHERE guid = $G" >/dev/null
OUT=$(mktemp)
# The EXPECTED VALUE (4th arg) is load-bearing: this watcher's own login relays field 194 carrying
# the pre-crossing NORMAL byte, and without it the watch passed on that and stopped before the
# RESTED flip it exists to prove.
timeout 30 "$WC" TEST Ginger values-watch "$G" 194 20 16777216 >"$OUT" 2>&1 &
WPID=$!
sleep 6
scall debug_check_rest_at $G 0 "$INN_X" "$INN_Y"
wait $WPID
if grep -q 'VALUES-WATCH PASS' "$OUT" && grep -q 'field 194 = 16777216' "$OUT"; then
  echo "  OK   wire: PLAYER_BYTES_2 field 194 = 16777216 (RESTED) relayed on live crossing"
else
  echo "  FAIL wire: rest byte not relayed on live crossing"; tail -3 "$OUT"; FAILED=1
fi
rm -f "$OUT"

# teardown
sqlq "UPDATE game_character SET resting = false, rested_since_micros = 0, rested_xp = 0 WHERE guid = $G" >/dev/null
sqlq "DELETE FROM game_rest_state_event WHERE character_guid = $G" >/dev/null
if [ "$FAILED" -eq 0 ]; then echo "[rest-state] PASS"; exit 0; else echo "[rest-state] FAIL"; exit 1; fi
