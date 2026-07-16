#!/usr/bin/env bash
# Cast-INTERRUPT wire test (headless). The completion sibling is test-cast-flow.sh; this one verifies the
# OTHER outcome: a hostile mob in MELEE range hits the caster mid-cast, so the gateway relays
# SMSG_SPELL_FAILURE (the cast-interrupt-on-damage signal, b4628b2 + the relay) and NO SMSG_SPELL_GO.
# This is the e2e proof of cast-interrupt that the spacetime-call guid-mangling blocked earlier.
#
# Prereqs: local STDB node + gateway up, TEST provisioned, Warlock "Ginger" (guid 5).
# Usage: tools/wire-client/test-cast-interrupt.sh [spell_id] [mob_entry]
set -uo pipefail
cd "$(dirname "$0")/../.."

source tools/wire-client/scenario-lib.sh
SPELL="${1:-686}"   # 686 = Shadow Bolt (1.7s timed)
ENTRY="${2:-103}"   # 103 = Garrick Padfoot (hostile Defias) — melee attacker
CHAR="Ginger"
CGUID=$(char_guid "$CHAR")
[ -z "$CGUID" ] && { echo "[test] character '$CHAR' not found in game_character" >&2; exit 1; }

cargo build -q -p wire-client || exit 1
TGT="$(mktemp -u /tmp/wc_target_XXXXXX)"; rm -f "$TGT"
# Run-scoped side-channel for the spawned mob's guid (work-item 161: no shared /tmp literal).
MOB_FILE=/tmp/wc_mobguid_$$
MOBGUID=""

orchestrate() {
  local GX="" GY="" MOB=""
  for _ in $(seq 1 30); do
    GX=$(spacetime sql "$DB" "SELECT x FROM game_world_entity WHERE guid=$CGUID" 2>&1 | grep -oE '\-?[0-9]+(\.[0-9]+)?' | tail -1)
    [ -n "$GX" ] && break; sleep 1
  done
  [ -z "$GX" ] && { echo "[orch] $CHAR never went live" >&2; return 1; }
  GY=$(spacetime sql "$DB" "SELECT y FROM game_world_entity WHERE guid=$CGUID" 2>&1 | grep -oE '\-?[0-9]+(\.[0-9]+)?' | tail -1)
  spacetime call "$DB" debug_spawn_at_feet $CGUID $ENTRY 1 >/dev/null 2>&1   # 1yd = immediate melee
  spacetime call "$DB" debug_set_health $CGUID 100000 >/dev/null 2>&1
  MOB=$(spacetime sql "$DB" "SELECT guid, x, y FROM game_world_entity WHERE entry=$ENTRY AND owner_guid=0" 2>&1 \
        | awk -F'|' -v gx="$GX" -v gy="$GY" '$1 ~ /[0-9]/ {g=$1;x=$2+0;y=$3+0;dx=x-gx;dy=y-gy;d=dx*dx+dy*dy; if(best==""||d<best){best=d;bg=g}} END{gsub(/ /,"",bg);print bg}')
  echo "$MOB" > "$MOB_FILE"
  # let the mob aggro + get onto its melee swing timer so a swing lands inside the 1.7s cast window
  sleep 4
  spacetime call "$DB" debug_set_health $CGUID 100000 >/dev/null 2>&1   # re-top after the first swings
  echo "[orch] interrupt target mob=$MOB (melee range, swinging)" >&2
  echo "$MOB" > "$TGT"
}

orchestrate &
ORCH=$!
WIRE_EXPECT_INTERRUPT=1 WIRE_TARGET_FILE="$TGT" timeout 120 cargo run -q -p wire-client -- TEST test123 "$CHAR" "$SPELL"
RC=$?
wait "$ORCH" 2>/dev/null || true
rm -f "$TGT"
# cleanup the debug-spawned mob
MOBGUID=$(cat "$MOB_FILE" 2>/dev/null || echo "")
[ -n "$MOBGUID" ] && spacetime sql "$DB" "DELETE FROM game_world_entity WHERE guid = $MOBGUID" >/dev/null 2>&1 || true
rm -f "$MOB_FILE"
exit $RC
