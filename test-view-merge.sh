#!/usr/bin/env bash
# #73 — cross-seam view-merge live acceptance (REBUILT: box-replace + per-shard RANGE rectangles,
# not per-cell handles — see docs/region-sharding.md's "View-merge across a seam" section and
# gateway/src/stdb/aoi.rs's module doc for the design #190's decline mandates). Manual verification
# run — WRITTEN, NOT executed by the implementing agent; live wire-client runs are operator-gated in
# attended sessions, docs/danger-zones.md "Verification". Kept close to test-aoi-relay.sh's (two
# concurrent wire sessions, command-FIFO driven) and test-warm-handoff.sh's (SEAMTEST account
# pattern, real seam geometry) shape.
#
# ⚠ GEOMETRY CAVEAT: the coordinates below are DERIVED from the documented live seam
# (test-warm-handoff.sh's comment + docs/region-sharding.md's worked example: region 2 = Goldshire,
# gx 530..545 / gy 330..345, owned by lyracore-world-2; everything else on map 0 is region 1 /
# lyracore), not re-verified against the live tables. Before running, confirm:
#   spacetime sql lyracore "SELECT * FROM game_map_region"
#   spacetime sql lyracore-realm "SELECT * FROM game_region_assignment"
# and adjust NEAR_LOGIN_X / WALK_TARGET_X / FAR_X below if the live menu has moved.
#
# ⚠ THE AWAY CONNECT IS ASYNC (#207 fast-follow 1, closed in the rebuild): NEAR's first crossing
# into the touching cell only KICKS OFF a background connect to lyracore-world-2 — it does not
# block on it. The away rectangles are subscribed on whichever later heartbeat sees that connect
# finish (still driven by every processed movement packet, not only a further cell crossing — see
# `AreaOfInterestTracker::update`'s doc comment), so `expect-create`'s existing 35s ack timeout is
# the assertion that this converges, not just that the first crossing worked instantly. If this
# script starts flaking on `expect-create` specifically, that budget — not the geometry — is the
# first thing to widen.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
WORLD2=lyracore-world-2
FAILED=0

# ---- geometry (map 0, all three points share gy335 — only gx/x varies) --------------------------
# grid_cell(x,y): gx = floor((17066.666 - x) / 50); +x world direction DECREASES gx (the aoi-observer
# "walk" verb only moves +x, so the NEAR observer must start on the LOW-gx side and walk toward the
# seam, ending just inside the box-touches-region2 window (anchor gx 528..532 touches cell 530)).
#
#   NEAR login   gx529  x=-9405  (home lyracore; box [527,531] already geometrically touches
#                                 530, but login stays home-only regardless — #73's own module doc)
#   NEAR walk-to gx528  x=-9350  (box [526,530] touches cell 530 — the first recenter after this
#                                 heartbeat is what activates view-merge: home's share of the box
#                                 splits into [526,529]x[full height] and away's into the single
#                                 touching column [530,530]x[full height] — one range query each,
#                                 never a per-cell subscribe)
#   FAR (peer)   gx530  x=-9450  (INSIDE the Goldshire rectangle -> home lyracore-world-2)
NEAR_LOGIN_X=-9405.0; WALK_TARGET_X=-9350.0; FAR_X=-9450.0
Y=300.0; Z=59.0

seamtest_fresh_char() { # $1=account $2=char $3=class $4=level  (password: adapters/lyracore/wire.sh)
  timeout 60 "$WC" "$1" "$2" char-delete >/dev/null 2>&1 || true
  timeout 60 "$WC" "$1" "$2" make-char "${3:-warrior}" >/dev/null 2>&1
  local guid; guid=$(char_guid "$2")
  [ -n "$guid" ] && scall debug_set_level "$guid" "${4:-5}"
  echo "$guid"
}

echo "== fresh chars (SEAMTEST / SEAMTEST2 — never TEST/TEST2, the shared-suite accounts)"
NEAR=$(seamtest_fresh_char SEAMTEST SeamNear warrior 5)
echo "near guid=$NEAR"
[ -n "$NEAR" ] || { echo "FAIL: no NEAR guid"; exit 1; }

FAR=$(seamtest_fresh_char SEAMTEST2 SeamFar warrior 5)
echo "far guid=$FAR"
[ -n "$FAR" ] || { echo "FAIL: no FAR guid"; exit 1; }

echo "== position NEAR (core side, at the pre-walk cell) and stage FAR's row inside Goldshire"
scall debug_teleport "$NEAR" 0 "$NEAR_LOGIN_X" "$Y" "$Z" 0
# FAR has never logged in yet at this point (fresh char) — stage the DURABLE row directly so its
# FIRST login's world-entry region resolver (docs/region-sharding.md §"How routing uses them") routes
# it straight to lyracore-world-2, exactly like test-aoi-relay.sh's re-entry step stages a position
# before a reconnect picks it up.
sqlq "UPDATE game_character SET map_id = 0, x = $FAR_X, y = $Y, z = $Z WHERE guid = $FAR" >/dev/null

echo "== pre-check: FAR's durable row lives on core (pre-first-login), nothing on world-2 yet"
sqlq "SELECT guid, map_id, x, y FROM game_character WHERE guid = $FAR" "$DB"
sqlq "SELECT guid FROM game_character WHERE guid = $FAR" "$WORLD2"

CMD_OBS=/tmp/wc_vm_cmd_$$; ACK_OBS=/tmp/wc_vm_ack_$$; OBS_READY=/tmp/wc_vm_obs_ready_$$
CMD_MOV=/tmp/wc_vm_mov_cmd_$$; MOV_READY=/tmp/wc_vm_mov_ready_$$
rm -f "$CMD_OBS" "$ACK_OBS" "$OBS_READY" "$CMD_MOV" "$MOV_READY"

echo "== leg 1: FAR logs in first (world entry resolves it onto world-2 — the away side)"
timeout 240 "$WC" SEAMTEST2 SeamFar aoi-mover "$CMD_MOV" "$MOV_READY" >/tmp/wc_vm_far.log 2>&1 &
MOVER=$!
wait_for_file 20 "$MOV_READY" || { echo "[orch] FAR never ready" >&2; cat /tmp/wc_vm_far.log; kill $MOVER 2>/dev/null; exit 1; }
rm -f "$MOV_READY"

echo "== post-login check: FAR's live entity is on world-2, not core"
sqlq "SELECT guid, map_id, grid_x, grid_y FROM game_world_entity WHERE guid = $FAR" "$WORLD2"
sqlq "SELECT guid FROM game_world_entity WHERE guid = $FAR" "$DB"

echo "== leg 2: NEAR logs in on core, still outside the straddle (login precondition: FAR not visible)"
timeout 240 "$WC" SEAMTEST SeamNear aoi-observer "$FAR" "$CMD_OBS" "$ACK_OBS" "$OBS_READY" >/tmp/wc_vm_near.log 2>&1 &
OBS=$!
wait_for_file 20 "$OBS_READY" || { echo "[orch] NEAR never ready (FAR visible at login?)" >&2; cat /tmp/wc_vm_near.log; kill $OBS $MOVER 2>/dev/null; exit 1; }
rm -f "$OBS_READY"
step_ok "login precondition: FAR (on world-2) not visible to NEAR (on core) before any recenter"

obs_cmd() {
  local ack; ack=$(obs_cmd_send "$1" "$CMD_OBS" "$ACK_OBS" 35)
  if grep -q "^OK" <<<"$ack"; then step_ok "NEAR: $1"; else step_fail "NEAR: $1 -> ${ack:-no ack}"; fi
}

echo "== the ask: NEAR walks toward the seam -> first recenter -> home/away rect split opens the away leg"
obs_cmd "walk $NEAR_LOGIN_X $Y $Z" # steps +x from NEAR_LOGIN_X, crossing into the touching cell
                                    # partway through — the walk verb's own fixed +60yd shape, not
                                    # WALK_TARGET_X directly (see the geometry comment above)

echo "== assertion 1 (AC): NEAR decodes FAR's CREATE — a single coherent crowd across the seam"
# expect-seen, not expect-create (found live, 2026-08-04): the away leg's resident sweep fires the
# moment the FIRST away-shard rect subscription applies, which is DURING the "walk" command's own
# 12-heartbeat loop above (that loop drains the socket after every send) — not after it, the way
# every OTHER use of this harness stages its trigger (a separate debug_teleport/burst issued AFTER
# the observer's own movement settles). expect-create deliberately clears any earlier sighting and
# waits for a FRESH create, so it times out here even though FAR's CREATE genuinely arrived — just
# a heartbeat or two earlier than this line. expect-seen asserts the same acceptance criterion
# (peer has been CREATEd — "already, or within the window") without assuming which side of the
# walk it happens on.
obs_cmd expect-seen

echo "== assertion 2 (AC): movement crosses the seam live, not on a refresh"
obs_cmd expect-move &
CMDPID=$!
sleep 1
echo "burst $FAR_X $Y $Z" > "$CMD_MOV"
wait $CMDPID

echo "== teardown"
echo "exit" > "$CMD_MOV"
wait "$MOVER" 2>/dev/null
echo "done" > "$CMD_OBS"
wait "$OBS"; OBS_RC=$?
[ $OBS_RC -ne 0 ] && { echo "[orch] NEAR observer exited rc=$OBS_RC"; tail -5 /tmp/wc_vm_near.log; FAILED=1; }

timeout 60 "$WC" SEAMTEST SeamNear char-delete >/dev/null 2>&1 || true
timeout 60 "$WC" SEAMTEST2 SeamFar char-delete >/dev/null 2>&1 || true
rm -f "$CMD_OBS" "$ACK_OBS" "$OBS_READY" "$CMD_MOV" "$MOV_READY"

if [ "$FAILED" -eq 0 ]; then echo "[view-merge] PASS"; exit 0; else echo "[view-merge] FAIL"; exit 1; fi

# ---- negative control (run separately — see docs/danger-zones.md §3 for the launch recipe) ------
# LYRACORE_VIEW_MERGE is read once per AreaOfInterestTracker::new(), which reads it from the GATEWAY
# PROCESS's own environment — it cannot be toggled without restarting the gateway. To prove the
# escape hatch actually gates the feature (not just documented as doing so):
#   1. pkill -x lyracore-gatewa; sleep 1
#   2. relaunch with the SAME recipe as docs/danger-zones.md §3 plus LYRACORE_VIEW_MERGE=0
#   3. re-run this script — "login precondition" must still pass, but `expect-create` (assertion 1)
#      must now FAIL/timeout: with the away leg disabled, NEAR's box never opens a connection to
#      world-2 at all, so FAR stays invisible even after the walk.
#   4. restart the gateway WITHOUT LYRACORE_VIEW_MERGE=0 (or omit the var — it defaults on) to restore
#      production behavior before leaving the stack running.
