#!/usr/bin/env bash
# test-walkmelee.sh — the walk_to movement helper (testing-hardening §3.3): walk from OUTSIDE the
# 5 yd standstill melee reach into range and prove the swing fires — i.e. the server tracks a real
# heartbeat-stream walk, which unlocks range/leash/AOI scenarios headlessly.
set -uo pipefail
cd "$(dirname "$0")/../.."
DB=spacetime-core
WC=./target/debug/wire-client
source tools/wire-client/scenario-lib.sh

GINGER=$(char_guid Ginger)
[ -z "$GINGER" ] && { echo "[walkmelee] no Ginger" >&2; exit 1; }

stay_start TEST test123 Ginger || exit 1
scall debug_teleport "$GINGER" 0 -8960.0 -440.0 81.0 0 # probed LoS-clear pad (danger-zones §2)
WOLF=$(spawn_at "$GINGER" 51000 2)
stay_stop
[ -z "$WOLF" ] && { echo "[walkmelee] wolf spawn failed" >&2; exit 1; }
WX=$(sql1 "SELECT x FROM game_world_entity WHERE guid = $WOLF")
WY=$(sql1 "SELECT y FROM game_world_entity WHERE guid = $WOLF")

RC=0
timeout 60 "$WC" TEST test123 Ginger walkmelee "$WOLF" "$WX" "$WY" 81.0 || RC=1

sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_creature_spawn WHERE entry = 51000" >/dev/null
sqlq "DELETE FROM game_world_entity WHERE guid = $WOLF" >/dev/null
if [ "$RC" = "0" ]; then echo "[walkmelee] PASS"; else echo "[walkmelee] FAIL"; exit 1; fi
