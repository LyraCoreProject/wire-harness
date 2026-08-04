#!/usr/bin/env bash
# #74 — proximity chat/emotes reach everyone who can see you across a seam. Manual verification
# run — WRITTEN, NOT executed by the implementing agent; live wire-client runs are operator-gated
# in attended sessions (docs/danger-zones.md "Verification"). FIFO-orchestration shape mined
# verbatim from test-view-merge.sh (#73's own acceptance script) — same SEAMTEST/SEAMTEST2
# accounts, same Goldshire seam, same aoi-observer/aoi-mover command-FIFO pattern. This script adds
# the "say"/"yell"/"emote" mover commands and the "expect-chat"/"expect-no-chat"/"expect-emote"
# observer commands (gateway/src/stdb/subscriptions.rs's `register_peer_view_relays` away-shard
# chat/emote relay, and tools/wire-client/src/modes/relay.rs's new command branches).
#
# ⚠ GEOMETRY CAVEAT (same as test-view-merge.sh): the coordinates below assume the documented live
# seam (region 2 = Goldshire, gx 530..545 / gy 330..345, owned by lyracore-world-2). Before
# running, confirm:
#   spacetime sql lyracore "SELECT * FROM game_map_region"
#   spacetime sql lyracore-realm "SELECT * FROM game_region_assignment"
# and adjust NEAR_LOGIN_X / FAR_X / OUT_OF_SAY_X below if the live menu has moved.
#
# ⚠ THE AWAY CONNECT IS ASYNC (#207 fast-follow 1): NEAR's first crossing into the touching cell
# only KICKS OFF a background connect to lyracore-world-2. `expect-seen`'s existing ack timeout
# (35s) is what lets this converge — see test-view-merge.sh's own note.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
WORLD2=lyracore-world-2
FAILED=0

# ---- geometry (map 0, all points share gy335 — only gx/x varies) --------------------------------
# grid_cell(x,y): gx = floor((17066.666 - x) / 50); +x world direction DECREASES gx (the aoi-observer
# "walk" verb only moves +x, in 12 x +5yd heartbeats -> +55yd total). DIFFERENT numbers from
# test-view-merge.sh: that script only needed FAR somewhere inside the box's away rect (any
# distance up to ~150-212yd — it was proving VISIBILITY). This script proves the 25yd SAY radius
# actually reaches across the seam and does NOT reach beyond it, which needs NEAR parked close
# enough to the region-2 boundary line (x ~ -9433.33, where gx flips 529 -> 530) that a 25yd probe
# on either side of it is geometrically possible at all — test-view-merge.sh's own anchor
# (finishing around gx528, ~90yd from the boundary) is too far back for that.
#
#   NEAR login   x=-9483  (gx530 at login — irrelevant: AreaOfInterestTracker::new() never
#                          resolves shards at login regardless of the box's gx, so FAR stays
#                          invisible pre-walk purely because it lives on a different database)
#   NEAR walk-to x~-9428  (start -9483, walk verb's fixed +55yd -> gx529, home lyracore;
#                          box [527,531] touches cell 530 -> away leg opens on lyracore-world-2)
#   FAR (SAY leg)      x=-9440  (gx530, home lyracore-world-2/Goldshire; ~12yd from NEAR's
#                                post-walk anchor -> inside the 25yd SAY range AND the merged view)
#   FAR (out-of-range) x=-9490  (gx531, still inside Goldshire's rect NEAR's away leg subscribes
#                                -> still in the merged VIEW; ~62yd from NEAR's anchor -> outside
#                                the 25yd SAY range. This is the assertion that matters: the away
#                                connection still carries EVERY game_chat_event row on
#                                lyracore-world-2, so "no delivery" here is the range filter
#                                working, not the peer being out of sight.)
NEAR_LOGIN_X=-9483.0
FAR_X=-9440.0        # ~12yd from NEAR's post-walk anchor (~-9428) -> in SAY range (25yd)
OUT_OF_SAY_X=-9490.0 # ~62yd from NEAR's post-walk anchor -> outside SAY range, still inside the view
Y=300.0; Z=59.0

seamtest_fresh_char() { # $1=account $2=password $3=char $4=class $5=level
  timeout 60 "$WC" "$1" "$2" "$3" char-delete >/dev/null 2>&1 || true
  timeout 60 "$WC" "$1" "$2" "$3" make-char "${4:-warrior}" >/dev/null 2>&1
  local guid; guid=$(char_guid "$3")
  [ -n "$guid" ] && scall debug_set_level "$guid" "${5:-5}"
  echo "$guid"
}

echo "== fresh chars (SEAMTEST / SEAMTEST2 — never TEST/TEST2, the shared-suite accounts)"
NEAR=$(seamtest_fresh_char SEAMTEST seamtest123 SeamNear warrior 5)
echo "near guid=$NEAR"
[ -n "$NEAR" ] || { echo "FAIL: no NEAR guid"; exit 1; }

FAR=$(seamtest_fresh_char SEAMTEST2 seamtest123 SeamFar warrior 5)
echo "far guid=$FAR"
[ -n "$FAR" ] || { echo "FAIL: no FAR guid"; exit 1; }

echo "== position NEAR (core side, near the boundary) and stage FAR's row inside Goldshire"
scall debug_teleport "$NEAR" 0 "$NEAR_LOGIN_X" "$Y" "$Z" 0
# FAR has never logged in yet — stage the DURABLE row so its FIRST login's world-entry region
# resolver routes it straight to lyracore-world-2 (test-view-merge.sh's own staging step).
sqlq "UPDATE game_character SET map_id = 0, x = $FAR_X, y = $Y, z = $Z WHERE guid = $FAR" >/dev/null

echo "== pre-check: FAR's durable row lives on core (pre-first-login), nothing on world-2 yet"
sqlq "SELECT guid, map_id, x, y FROM game_character WHERE guid = $FAR"
sqlq "SELECT guid FROM game_character WHERE guid = $FAR" "$WORLD2"

CMD_OBS=/tmp/wc_sc_cmd_$$; ACK_OBS=/tmp/wc_sc_ack_$$; OBS_READY=/tmp/wc_sc_obs_ready_$$
CMD_MOV=/tmp/wc_sc_mov_cmd_$$; MOV_READY=/tmp/wc_sc_mov_ready_$$
rm -f "$CMD_OBS" "$ACK_OBS" "$OBS_READY" "$CMD_MOV" "$MOV_READY"

echo "== leg 1: FAR logs in first (world entry resolves it onto world-2 — the away side)"
timeout 240 "$WC" SEAMTEST2 seamtest123 SeamFar aoi-mover "$CMD_MOV" "$MOV_READY" >/tmp/wc_sc_far.log 2>&1 &
MOVER=$!
wait_for_file 20 "$MOV_READY" || { echo "[orch] FAR never ready" >&2; cat /tmp/wc_sc_far.log; kill $MOVER 2>/dev/null; exit 1; }
rm -f "$MOV_READY"

echo "== post-login check: FAR's live entity is on world-2, not core"
sqlq "SELECT guid, map_id, grid_x, grid_y FROM game_world_entity WHERE guid = $FAR" "$WORLD2"
sqlq "SELECT guid FROM game_world_entity WHERE guid = $FAR"

echo "== leg 2: NEAR logs in on core, still outside the straddle (login precondition: FAR not visible)"
timeout 240 "$WC" SEAMTEST seamtest123 SeamNear aoi-observer "$FAR" "$CMD_OBS" "$ACK_OBS" "$OBS_READY" >/tmp/wc_sc_near.log 2>&1 &
OBS=$!
wait_for_file 20 "$OBS_READY" || { echo "[orch] NEAR never ready (FAR visible at login?)" >&2; cat /tmp/wc_sc_near.log; kill $OBS $MOVER 2>/dev/null; exit 1; }
rm -f "$OBS_READY"
step_ok "login precondition: FAR (on world-2) not visible to NEAR (on core) before any recenter"

obs_cmd() {
  local ack; ack=$(obs_cmd_send "$1" "$CMD_OBS" "$ACK_OBS" 35)
  if grep -q "^OK" <<<"$ack"; then step_ok "NEAR: $1"; else step_fail "NEAR: $1 -> ${ack:-no ack}"; fi
}

echo "== setup: NEAR walks toward the seam -> away leg opens against lyracore-world-2"
obs_cmd "walk $NEAR_LOGIN_X $Y $Z"

echo "== assertion 0 (precondition): NEAR's merged view already includes FAR (proven by #73 — a re-check here means a chat failure below is a CHAT bug, not a stale view-merge regression)"
obs_cmd expect-seen

echo "== assertion 1 (AC): FAR /says within SAY range -> NEAR decodes SMSG_MESSAGECHAT with the right text (sender guid = FAR) despite FAR living on a DIFFERENT database"
SAY_PROBE="seam-say-$$-$(date +%s)"
obs_cmd "expect-chat $SAY_PROBE" &
CMDPID=$!
sleep 1
echo "say $SAY_PROBE" > "$CMD_MOV"
wait $CMDPID

echo "== assertion 2 (AC): a text emote crosses likewise, WITH the cross-shard target name resolved (character_anywhere — #22's whisper fix, reused for #74's server-side name bake-in)"
obs_cmd "expect-emote SeamNear" &
CMDPID=$!
sleep 1
echo "emote $NEAR" > "$CMD_MOV"
wait $CMDPID

echo "== move FAR beyond SAY range, still within NEAR's merged VIEW, for the negative control. Via the mover's OWN movement (not an admin debug_teleport) — FAR's live row is on lyracore-world-2 now (post-login world-entry routing), and 'burst' already speaks to whichever database this connection is on"
echo "burst $OUT_OF_SAY_X $Y $Z" > "$CMD_MOV"
sleep 3 # let the burst's heartbeats land and the entity update settle before the next SAY

echo "== assertion 3 (AC, the broadcast-leak check): FAR /says beyond SAY range -> NO delivery to NEAR, even though FAR is still inside the merged view and the away connection still carries every game_chat_event row on that database"
OUT_OF_RANGE_PROBE="seam-out-of-range-$$-$(date +%s)"
echo "say $OUT_OF_RANGE_PROBE" > "$CMD_MOV"
sleep 1
obs_cmd "expect-no-chat 5"

echo "== teardown"
echo "exit" > "$CMD_MOV"
wait "$MOVER" 2>/dev/null
echo "done" > "$CMD_OBS"
wait "$OBS"; OBS_RC=$?
[ $OBS_RC -ne 0 ] && { echo "[orch] NEAR observer exited rc=$OBS_RC"; tail -5 /tmp/wc_sc_near.log; FAILED=1; }

timeout 60 "$WC" SEAMTEST seamtest123 SeamNear char-delete >/dev/null 2>&1 || true
timeout 60 "$WC" SEAMTEST2 seamtest123 SeamFar char-delete >/dev/null 2>&1 || true
rm -f "$CMD_OBS" "$ACK_OBS" "$OBS_READY" "$CMD_MOV" "$MOV_READY"

if [ "$FAILED" -eq 0 ]; then echo "[seam-chat] PASS"; exit 0; else echo "[seam-chat] FAIL"; exit 1; fi

# ---- negative control (run separately — see docs/danger-zones.md §3 for the launch recipe) ------
# Same escape-hatch shape as test-view-merge.sh's own negative control: LYRACORE_VIEW_MERGE is read once
# per AreaOfInterestTracker::new() from the GATEWAY PROCESS's environment, so it cannot be toggled
# without a restart.
#   1. pkill -x lyracore-gatewa; sleep 1
#   2. relaunch with the SAME recipe as docs/danger-zones.md §3 plus LYRACORE_VIEW_MERGE=0
#   3. re-run this script — "login precondition" and "expect-seen" (assertion 0) must now FAIL/
#      timeout (the away leg never opens at all — #73's own contract), which means assertions 1-3
#      never get a chance to run either. This is the intended degrade: LYRACORE_VIEW_MERGE=0 collapses
#      the WHOLE cross-seam chat feature along with view-merge itself, not just the range filter.
#   4. restart the gateway WITHOUT LYRACORE_VIEW_MERGE=0 (or omit the var — it defaults on) to restore
#      production behavior before leaving the stack running.
