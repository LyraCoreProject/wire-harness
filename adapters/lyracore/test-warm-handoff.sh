#!/usr/bin/env bash
# #72 slice 2 — live warm-handoff acceptance. Manual verification run (not yet wired into any
# test-loop convention); kept close to the other test-*.sh orchestrators' shape.
set -u
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
. "$ADAPTER_DIR/scenario-lib.sh"
# $WC comes from scenario-lib.sh (the adapters/lyracore/wire.sh seam) — do not re-point it at the binary.
DB=${DB:-lyracore}
WORLD2=lyracore-world-2

# The live seam (docs/region-sharding.md / SESSION-HANDOFF): region 2 (Goldshire, gx 530-545,
# gy 330-345) owned by lyracore-world-2; the rest of map 0 (region 1) by lyracore.
# Two real imported spawns straddling the boundary, ~126yd apart:
#   region1 (core):  x=-9424.66 y=129.056  z=59.8005  (gx529)
#   region2 (world2): x=-9549.82 y=112.997 z=59.0065  (gx533 — several cells past the seam)
SRC_X=-9424.66; SRC_Y=129.056; SRC_Z=59.8005
DST_X=-9549.82; DST_Y=112.997; DST_Z=59.0065

# The account here MUST be the one the wire legs below log in with (SEAMTEST): the shared
# fresh_char/drop_char helpers are pinned to the suite's TEST account by convention, and a char
# created there is invisible to SEAMTEST's char enum (bit 2026-08-03).
seamtest_fresh_char() {
  timeout 60 "$WC" SEAMTEST "$1" char-delete >/dev/null 2>&1 || true
  timeout 60 "$WC" SEAMTEST "$1" make-char "${2:-warrior}" >/dev/null 2>&1
  local guid; guid=$(char_guid "$1")
  [ -n "$guid" ] && scall debug_set_level "$guid" "${3:-5}"
  echo "$guid"
}
echo "== fresh char"
GUID=$(seamtest_fresh_char SeamHandoff warrior 5)
echo "guid=$GUID"
[ -n "$GUID" ] || { echo "FAIL: no guid"; exit 1; }

echo "== pre-position at the region1 side of the seam"
scall debug_teleport "$GUID" 0 "$SRC_X" "$SRC_Y" "$SRC_Z" 0

echo "== pre-check: durable row lives on core, nothing on world-2"
sqlq "SELECT map_id, x, y FROM game_character WHERE guid = $GUID" "$DB"
sqlq "SELECT guid FROM game_character WHERE guid = $GUID" "$WORLD2"

echo "== leg 1: log in on core and walk across the seam (expect-handoff)"
timeout 90 "$WC" SEAMTEST SeamHandoff seamwalk "$SRC_X" "$SRC_Y" "$SRC_Z" "$DST_X" "$DST_Y" "$DST_Z" expect-handoff
RC=$?
echo "seamwalk rc=$RC"

echo "== post-check: durable row"
echo "-- core:"
sqlq "SELECT guid, map_id, x, y FROM game_character WHERE guid = $GUID" "$DB"
echo "-- world-2:"
sqlq "SELECT guid, map_id, x, y FROM game_character WHERE guid = $GUID" "$WORLD2"

echo "== escrow ledgers (both must be empty for this guid)"
echo "-- core out/in:"
sqlq "SELECT * FROM game_transfer_out WHERE character_guid = $GUID" "$DB"
sqlq "SELECT * FROM game_transfer_in WHERE character_guid = $GUID" "$DB"
echo "-- world-2 out/in:"
sqlq "SELECT * FROM game_transfer_out WHERE character_guid = $GUID" "$WORLD2"
sqlq "SELECT * FROM game_transfer_in WHERE character_guid = $GUID" "$WORLD2"

echo "== leg 2: re-login (bare M1 smoke — no mode) — must land on world-2 with NO second transfer"
timeout 30 "$WC" SEAMTEST SeamHandoff
RC2=$?
echo "relogin rc=$RC2"

echo "== teardown"
timeout 60 "$WC" SEAMTEST SeamHandoff char-delete >/dev/null 2>&1 || true
