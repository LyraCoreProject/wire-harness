#!/usr/bin/env bash
# Cast-flow integration test (headless, no wine client).
#
# Drives the headless wire test-client: log in as a Warlock, spawn a hostile mob at her
# feet, cast a TIMED spell at it, and assert the SMSG cast sequence the gateway relays:
#   SMSG_SPELL_START(cast_time) -> SMSG_SPELL_GO(targets.unit=mob) [NO 2nd START — the timed
#   completion sends GO alone] -> [SMSG_SPELLNONMELEEDAMAGELOG] -> SMSG_SPELL_COOLDOWN
# i.e. it deterministically verifies the cast-state-close (the START->GO pair the 5875
# client needs) and the projectile trajectory (GO.unit_target) — the class of bug we used
# to QA by hand through the wine client.
#
# Prereqs: local STDB node + gateway up, the TEST account provisioned
# (`cargo run -p spacetime-core-gateway -- provision TEST test123`), and a Warlock named
# "Ginger" (the client creates it on first run). Exits nonzero on assertion failure.
#
# Usage: tools/wire-client/test-cast-flow.sh [spell_id] [mob_entry]
set -uo pipefail
cd "$(dirname "$0")/../.."

source tools/wire-client/scenario-lib.sh
SPELL="${1:-686}"   # 686 = Shadow Bolt (1.7s timed projectile)
ENTRY="${2:-103}"   # 103 = Garrick Padfoot (a hostile Defias) — the cast target
CHAR="Ginger"
CGUID=$(char_guid "$CHAR")
[ -z "$CGUID" ] && { echo "[test] character '$CHAR' not found in game_character" >&2; exit 1; }

cargo build -q -p wire-client || exit 1
TGT="$(mktemp -u /tmp/wc_target_XXXXXX)"; rm -f "$TGT"

# Purge any stale spawned mobs of this entry near the Northshire start (keeps the Goldshire
# quest spawn at x < -9100 untouched).
spacetime sql "$DB" "DELETE FROM game_world_entity WHERE entry=$ENTRY AND owner_guid=0 AND x>-9100 AND x<-8800 AND y>-300 AND y<50" >/dev/null 2>&1 || true

# Orchestrator: wait for the caster to be in-world, spawn the target at her feet, keep her
# alive, then hand the wire client the target's exact guid (decoded-precise, no mangling).
orchestrate() {
  local GX="" GY="" MOB=""
  for _ in $(seq 1 30); do
    GX=$(spacetime sql "$DB" "SELECT x FROM game_world_entity WHERE guid=$CGUID" 2>&1 | grep -oE '\-?[0-9]+(\.[0-9]+)?' | tail -1)
    [ -n "$GX" ] && break
    sleep 1
  done
  [ -z "$GX" ] && { echo "[orch] $CHAR never went live" >&2; return 1; }
  GY=$(spacetime sql "$DB" "SELECT y FROM game_world_entity WHERE guid=$CGUID" 2>&1 | grep -oE '\-?[0-9]+(\.[0-9]+)?' | tail -1)
  spacetime call "$DB" debug_spawn_at_feet $CGUID $ENTRY 8 >/dev/null 2>&1
  sleep 1
  spacetime call "$DB" debug_set_health $CGUID 100000 >/dev/null 2>&1   # survive the cast
  MOB=$(spacetime sql "$DB" "SELECT guid, x, y FROM game_world_entity WHERE entry=$ENTRY AND owner_guid=0" 2>&1 \
        | awk -F'|' -v gx="$GX" -v gy="$GY" '$1 ~ /[0-9]/ {g=$1;x=$2+0;y=$3+0;dx=x-gx;dy=y-gy;d=dx*dx+dy*dy; if(best==""||d<best){best=d;bg=g}} END{gsub(/ /,"",bg);print bg}')
  echo "[orch] target mob=$MOB near $CHAR=($GX,$GY)" >&2
  echo "$MOB" > "$TGT"
}

orchestrate &
ORCH=$!
WIRE_TARGET_FILE="$TGT" timeout 120 cargo run -q -p wire-client -- TEST test123 "$CHAR" "$SPELL"
RC=$?
wait "$ORCH" 2>/dev/null || true
rm -f "$TGT"
exit $RC
