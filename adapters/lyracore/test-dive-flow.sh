#!/usr/bin/env bash
# Operator-run breath acceptance: submerge, take one drowning tick, and surface.
#
# Usage: printf '%s\n' "$PASSWORD" | \
#   LYRACORE_DIR=/path/to/LyraCore bash adapters/lyracore/test-dive-flow.sh \
#   <account> <DiveCharacter> <map_id> <x> <y> <surface_z> <submerged_z>
#
# The named Character must be disposable and its name must start with "Dive". This script deletes
# and recreates it. Issue #142 assigns Realm execution and coordinate selection to the Operator.
set -uo pipefail

usage() {
  echo "usage: test-dive-flow.sh <account> <DiveCharacter> <map_id> <x> <y> <surface_z> <submerged_z> (password on stdin)" >&2
  exit 2
}

[ "$#" -eq 7 ] || usage
ACCOUNT=$1
CHARACTER=$2
MAP_ID=$3
X=$4
Y=$5
SURFACE_Z=$6
SUBMERGED_Z=$7
case "$ACCOUNT" in (*[!A-Za-z0-9_]*|'') usage ;; esac
case "$CHARACTER" in (*[!A-Za-z0-9]*|'') usage ;; esac
case "$CHARACTER" in (Dive*) ;; (*) usage ;; esac
case "$MAP_ID" in (*[!0-9]*|'') usage ;; esac
[ "${#ACCOUNT}" -le 32 ] || usage
[ "${#CHARACTER}" -ge 5 ] && [ "${#CHARACTER}" -le 12 ] || usage
[ "$MAP_ID" -le 4294967295 ] 2>/dev/null || usage
case "${DB:-lyracore}" in (-*|*[!A-Za-z0-9_-]*|'') echo "[dive] invalid DB name" >&2; exit 2 ;; esac

valid_coord() {
  awk -v value="$1" 'BEGIN {
    if (value !~ /^-?([0-9]+([.][0-9]*)?|[.][0-9]+)([eE][+-]?[0-9]+)?$/) exit 1
    number = value + 0
    exit !(number >= -17066.666 && number <= 17066.666)
  }'
}
for coord in "$X" "$Y" "$SURFACE_Z" "$SUBMERGED_Z"; do
  valid_coord "$coord" || usage
done
awk -v surface="$SURFACE_Z" -v submerged="$SUBMERGED_Z" \
  'BEGIN { exit !(surface > submerged) }' || usage

IFS= read -r PASSWORD || { echo "[dive] password is required on stdin" >&2; exit 2; }
[ -n "$PASSWORD" ] || { echo "[dive] password is required on stdin" >&2; exit 2; }

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh"
source "$ADAPTER_DIR/scenario-lib.sh"

PASSWORD_VAR="WIRE_PASSWORD_$ACCOUNT"
export "$PASSWORD_VAR=$PASSWORD"
unset PASSWORD
export WIRE_CLASS=warrior

WIRE_PID=""
CHAR_GUID=""
CHAR_OWNED=0
cleanup() {
  [ -n "$WIRE_PID" ] && kill "$WIRE_PID" 2>/dev/null || true
  [ -n "$WIRE_PID" ] && wait "$WIRE_PID" 2>/dev/null || true
  [ -n "${SC_STAY_PID:-}" ] && stay_stop || true
  [ "$CHAR_OWNED" -eq 1 ] && timeout 60 "$WC" "$ACCOUNT" "$CHARACTER" char-delete >/dev/null 2>&1 || true
}
trap cleanup EXIT

# Keep these constants aligned with crates/lyracore-shared/src/terrain.rs. The offline adapter test
# fixes a known coordinate to its expected cell so an accidental arithmetic change is visible.
CELL_X=$(awk -v value="$X" 'BEGIN { print int((17066.666 - value) / (533.3333 / 16.0)) }')
CELL_Y=$(awk -v value="$Y" 'BEGIN { print int((17066.666 - value) / (533.3333 / 16.0)) }')

# This is the only remote phase allowed before the disposable Character is changed. Swimming flags
# alone do not establish submersion: the imported cell and its liquid height must support both the
# submerged and surface positions using LyraCore's fixed 2-yard head height.
CELL_COUNT=$(sql1 "SELECT COUNT(*) AS n FROM game_terrain_chunk WHERE map_id = $MAP_ID AND cell_x = $CELL_X AND cell_y = $CELL_Y")
[ "$CELL_COUNT" = "1" ] || {
  echo "[dive] imported terrain cell map=$MAP_ID cell=($CELL_X,$CELL_Y) is missing or duplicated" >&2
  exit 1
}
HAS_LIQUID=$(sql1 "SELECT has_liquid FROM game_terrain_chunk WHERE map_id = $MAP_ID AND cell_x = $CELL_X AND cell_y = $CELL_Y")
[ "$HAS_LIQUID" = "true" ] || {
  echo "[dive] imported terrain cell map=$MAP_ID cell=($CELL_X,$CELL_Y) has no liquid" >&2
  exit 1
}
LIQUID_LEVEL=$(sql1 "SELECT liquid_level FROM game_terrain_chunk WHERE map_id = $MAP_ID AND cell_x = $CELL_X AND cell_y = $CELL_Y")
valid_coord "$LIQUID_LEVEL" || { echo "[dive] imported liquid level is invalid" >&2; exit 1; }
awk -v liquid="$LIQUID_LEVEL" -v submerged="$SUBMERGED_Z" -v surface="$SURFACE_Z" \
  'BEGIN { exit !((submerged + 2) < liquid && (surface + 2) >= liquid) }' || {
  echo "[dive] coordinates do not cross imported liquid level $LIQUID_LEVEL with the 2-yard head height" >&2
  exit 1
}

wire_build || exit 1
timeout 60 "$WC" "$ACCOUNT" "$CHARACTER" char-delete >/dev/null 2>&1 || {
  echo "[dive] could not clear disposable Character $CHARACTER" >&2
  exit 1
}
timeout 60 "$WC" "$ACCOUNT" "$CHARACTER" make-char warrior >/dev/null || {
  echo "[dive] could not create disposable Character $CHARACTER" >&2
  exit 1
}
CHAR_GUID=$(char_guid "$CHARACTER")
[ -n "$CHAR_GUID" ] || { echo "[dive] failed to create $CHARACTER" >&2; exit 1; }
CHAR_OWNED=1
scall debug_set_level "$CHAR_GUID" 5 || { echo "[dive] debug_set_level failed" >&2; exit 1; }
sqlq "UPDATE game_character SET map_id = $MAP_ID, x = $X, y = $Y, z = $SURFACE_Z WHERE guid = $CHAR_GUID" >/dev/null || {
  echo "[dive] could not stage the surface position" >&2
  exit 1
}
INITIAL_HEALTH=$(sql1 "SELECT health FROM game_character WHERE guid = $CHAR_GUID")
case "$INITIAL_HEALTH" in (*[!0-9]*|'') echo "[dive] staged Character health is invalid" >&2; exit 1 ;; esac
[ "$INITIAL_HEALTH" -gt 0 ] || { echo "[dive] staged Character is not alive" >&2; exit 1; }

timeout 150 "$WC" "$ACCOUNT" "$CHARACTER" scenario-dive \
  "$X" "$Y" "$SUBMERGED_Z" "$X" "$Y" "$SURFACE_Z" &
WIRE_PID=$!
wait "$WIRE_PID"
WIRE_RC=$?
WIRE_PID=""
[ "$WIRE_RC" -eq 0 ] || { echo "[dive] wire scenario failed" >&2; exit "$WIRE_RC"; }

# Logout persistence is asynchronous. Require both damage and the final surface position from the
# durable Character row. Packet-level timer and damage identity are checked inside scenario-dive.
FINAL_HEALTH=""
FINAL_POSITION=""
for _ in $(seq 1 10); do
  FINAL_HEALTH=$(sql1 "SELECT health FROM game_character WHERE guid = $CHAR_GUID")
  FINAL_POSITION=$(sqlq "SELECT x, y, z FROM game_character WHERE guid = $CHAR_GUID" | sed -n 3p)
  if awk -F'|' -v want_x="$X" -v want_y="$Y" -v want_z="$SURFACE_Z" '
    function abs(v) { return v < 0 ? -v : v }
    { gsub(/ /, "", $1); gsub(/ /, "", $2); gsub(/ /, "", $3);
      exit !(abs($1-want_x) <= 0.1 && abs($2-want_y) <= 0.1 && abs($3-want_z) <= 0.1) }
  ' <<<"$FINAL_POSITION" &&
    [ -n "$FINAL_HEALTH" ] && [ "$FINAL_HEALTH" -gt 0 ] 2>/dev/null &&
    [ "$FINAL_HEALTH" -lt "$INITIAL_HEALTH" ] 2>/dev/null; then
    echo "[dive] PASS: health $INITIAL_HEALTH->$FINAL_HEALTH persisted at the surface position"
    exit 0
  fi
  sleep 1
done

echo "[dive] durable check failed: health $INITIAL_HEALTH->$FINAL_HEALTH, position=$FINAL_POSITION" >&2
exit 1
