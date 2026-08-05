#!/usr/bin/env bash
# test-atwar.sh — 195 slice B: the rep pane's At-War checkbox round-trips and PERSISTS RELOG.
# Flow: seed a standing row on the fixture faction (50900, rep-index 60) -> wire session sends
# CMSG_SET_FACTION_ATWAR(60, on) -> relog probe asserts SMSG_INITIALIZE_FACTIONS slot 60 carries
# the AT_WAR flag bit -> flip it off -> relog probe asserts the bit cleared. Fully headless.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
DB=${DB:-lyracore}
# $WC comes from scenario-lib.sh (the adapters/lyracore/wire.sh seam) — do not re-point it at the binary.
source "$ADAPTER_DIR/scenario-lib.sh"

GINGER=$(char_guid Ginger)
[ -z "$GINGER" ] && { echo "[atwar] no Ginger" >&2; exit 1; }
IDX=60 # the scenario fixture faction 50900's reputation_index (debug_seed_scenario_fixtures)
FAILED=0

# Seed/normalize the standing row (the checkbox needs a row; grant 0 upserts at base_standing).
scall debug_grant_reputation "$GINGER" 50900 0
STANDING=$(sql1 "SELECT standing FROM game_player_reputation WHERE character_guid = $GINGER AND faction_id = 50900")
[ -z "$STANDING" ] && { echo "[atwar] no standing row after grant — fixture faction missing?" >&2; exit 1; }

# ON: send the checkbox, then assert the flag survives a relog.
timeout 60 "$WC" TEST Ginger atwar $IDX 1 || { echo "[atwar] send(on) failed" >&2; FAILED=1; }
AW=$(sql1 "SELECT COUNT(*) AS n FROM game_player_reputation WHERE character_guid = $GINGER AND faction_id = 50900 AND at_war = true")
assert_ge "server row: at_war = true after the CMSG" "${AW:-0}" 1
timeout 60 "$WC" TEST Ginger init-factions $IDX "$STANDING" 1 \
  && step_ok "relog: INITIALIZE_FACTIONS slot $IDX carries AT_WAR" \
  || { echo "[atwar] STEP-ASSERT FAIL: AT_WAR flag missing on relog" >&2; FAILED=1; }

# OFF: uncheck, assert the bit clears on the next relog (round-trip both directions).
timeout 60 "$WC" TEST Ginger atwar $IDX 0 || { echo "[atwar] send(off) failed" >&2; FAILED=1; }
timeout 60 "$WC" TEST Ginger init-factions $IDX "$STANDING" 0 \
  && step_ok "relog: AT_WAR cleared after uncheck" \
  || { echo "[atwar] STEP-ASSERT FAIL: AT_WAR flag stuck on after uncheck" >&2; FAILED=1; }

if [ "$FAILED" = "0" ]; then echo "[atwar] PASS"; else echo "[atwar] FAIL"; exit 1; fi
