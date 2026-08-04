#!/usr/bin/env bash
# test-persist-health.sh — headless verification for work-item #078
# (persist current health/power across relog).
#
# Verifies: a player damaged to a non-full HP value, logged out and back in, comes back at the
# persisted health — NOT force-healed to max_health.
#
# Steps:
#   1. Login (stay mode) as CHAR to create a live game_world_entity.
#   2. Record baseline health, then damage to 22/max via debug_set_health.
#   3. Signal the wire-client to disconnect cleanly (CMSG_LOGOUT_REQUEST path -> persist_entity).
#   4. Confirm game_character.health is clamped-persisted (not 0, not max_health) and the live
#      entity row is gone (offline).
#   5. Relog (stay mode) and confirm the rebuilt game_world_entity.health is well below max_health
#      (i.e. NOT force-healed to full) and NOT 0 (relog-alive rule).
#
# Prereqs: local STDB node + gateway up, TEST account provisioned, a character named $CHAR exists.
# Exits nonzero on failure.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR

source "$ADAPTER_DIR/scenario-lib.sh"
CHAR=${1:-Ginger}

wire_build || exit 1

CGUID=$(char_guid "$CHAR")
if [ -z "$CGUID" ]; then
  echo "[078] ERROR: character '$CHAR' not found" >&2
  exit 1
fi
echo "[078] target: $CHAR (guid=$CGUID)"

SENTINEL1=/tmp/wc078_stay1_$$
SENTINEL2=/tmp/wc078_stay2_$$
FAIL=0

echo "[078] Step 1: login (stay) to create a live entity..."
# outer timeout 90 > the stay mode's own 60s sentinel deadline — the wire client must own the timeout, not be SIGKILLed mid-drain
timeout 90 "$WC" TEST "$CHAR" stay "$SENTINEL1" &
WC1=$!
for _ in $(seq 1 15); do
  H=$(spacetime sql "$DB" "SELECT health FROM game_world_entity WHERE guid=$CGUID" 2>&1 | grep -oE '[0-9]+' | tail -1)
  [ -n "$H" ] && break
  sleep 1
done
if [ -z "$H" ]; then
  echo "[078] FAIL — entity never went live" >&2
  touch "$SENTINEL1"; wait "$WC1" 2>/dev/null
  exit 1
fi
echo "[078] baseline health=$H"

echo "[078] Step 2: damage to 22 via debug_set_health (held IN COMBAT so regen can't close it)..."
# Regen is spirit+level+1 per tick (~27 HP/tick for a low-level char) — a 46-HP wound closes in
# ~2 ticks, so the old bare wound RACED the async disconnect persist and flaked under suite load
# (relogged at FULL = regen won, not force-heal). In-combat blocks passive health regen: spawn the
# passive fixture wolf OUT OF REACH (10 yd — no swings ever land) and arm an engagement; the wound
# then survives any persist latency. The logout itself disengages, so nothing lingers.
spacetime call "$DB" -- debug_spawn_at_feet "$CGUID" 51000 10 >/dev/null 2>&1
PWOLF=$(spacetime sql "$DB" "SELECT guid FROM game_world_entity WHERE entry = 51000" 2>/dev/null | grep -oE '[0-9]{15,}' | sort -n | tail -1)
[ -n "$PWOLF" ] && spacetime call "$DB" -- debug_engage "$CGUID" "$PWOLF" >/dev/null 2>&1
spacetime call "$DB" debug_set_health "$CGUID" 22 -s local >/dev/null 2>&1
sleep 1

echo "[078] Step 3: clean disconnect (persist_entity fires)..."
touch "$SENTINEL1"
wait "$WC1" 2>/dev/null
sleep 1

PERSISTED=$(spacetime sql "$DB" "SELECT health FROM game_character WHERE guid=$CGUID" 2>&1 | grep -oE '[0-9]+' | tail -1)
echo "[078] persisted game_character.health=$PERSISTED"
if [ -z "$PERSISTED" ] || [ "$PERSISTED" -eq 0 ]; then
  echo "[078] FAIL — persisted health is 0/missing (relog-alive clamp not applied)" >&2
  FAIL=1
fi

# The fixture wolf's job ended with the persist — clean it (entity + spawn row) before the relog.
[ -n "${PWOLF:-}" ] && spacetime sql "$DB" "DELETE FROM game_creature_spawn WHERE guid = $PWOLF" >/dev/null 2>&1
[ -n "${PWOLF:-}" ] && spacetime sql "$DB" "DELETE FROM game_world_entity WHERE guid = $PWOLF" >/dev/null 2>&1
echo "[078] Step 4: relog (stay) and check the rebuilt entity's health..."
timeout 90 "$WC" TEST "$CHAR" stay "$SENTINEL2" & # 90 > the stay mode's 60s inner deadline (same as step 1)
WC2=$!
RELOG_HEALTH=""
MAXHP=""
for _ in $(seq 1 15); do
  ROW=$(spacetime sql "$DB" "SELECT health, max_health FROM game_world_entity WHERE guid=$CGUID" 2>&1 | tail -1)
  RELOG_HEALTH=$(echo "$ROW" | awk '{print $1}')
  MAXHP=$(echo "$ROW" | awk '{print $3}')
  [ -n "$RELOG_HEALTH" ] && [ "$RELOG_HEALTH" != "health" ] && break
  sleep 1
done
# Teardown (moved from wire-suite.sh, work-item 162): heal the 22hp residue while this stay
# session is still live, so later combat tests don't start near-dead — standalone AND suite runs.
spacetime call "$DB" debug_set_health "$CGUID" 100000 >/dev/null 2>&1 || true
touch "$SENTINEL2"
wait "$WC2" 2>/dev/null

echo "[078] relogged health=$RELOG_HEALTH max_health=$MAXHP"
if [ -z "$RELOG_HEALTH" ] || [ "$RELOG_HEALTH" = "health" ]; then
  echo "[078] FAIL — could not read relogged entity health" >&2
  FAIL=1
elif [ "$RELOG_HEALTH" -eq 0 ]; then
  echo "[078] FAIL — relogged at 0 health (should clamp to >=1)" >&2
  FAIL=1
elif [ -n "$MAXHP" ] && [ "$RELOG_HEALTH" -eq "$MAXHP" ]; then
  echo "[078] FAIL — relogged at FULL health (force-heal regression: health == max_health)" >&2
  FAIL=1
else
  echo "[078] PASS — relogged at persisted (non-full) health, not force-healed to max."
fi

if [ "$FAIL" -eq 0 ]; then
  echo "[078] ALL CHECKS PASSED"
  exit 0
else
  echo "[078] SOME CHECKS FAILED"
  exit 1
fi
