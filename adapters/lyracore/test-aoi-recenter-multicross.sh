#!/usr/bin/env bash
# 190 rework (batched generational diff): the observer walks MULTIPLE cell crossings in DIFFERENT
# directions in one command ("walk-multi" — E, N, W, S, a loop back to the start) instead of the
# single +x crossing test-aoi-relay.sh's "walk" step exercises. This is the generational scheme's
# own worst case: the W leg's entering generation is exactly the E leg's generation retiring (a
# full-strip reversal), and the S leg does the same for N — see gateway/src/stdb/aoi.rs's module
# doc for the retirement proof this drives at the wire level.
#
# Asserts:
#   1. peer (already visible, staged near the loop's center) sees ZERO create/destroy DURING the
#      loop — any would mean a flicker (duplicate coverage) or a gap (a cell dropped early), the
#      exact failure class #203's per-cell rewrite introduced around recenters. Checked by
#      "walk-multi" itself (relay.rs bails non-zero on any).
#   2. the mover's MSG_MOVE_HEARTBEATs still relay to the observer AFTER the whole loop — the
#      #109-class continued-relay check, now proven across several crossings/directions instead of
#      just one.
# Requires the gateway running with LYRACORE_AOI=1.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"
scenario_preflight aoi-recenter-multicross
DFS=$(char_guid dfsdfsd)
[ -z "$DFS" ] && timeout 60 "$WC" TEST2 dfsdfsd logout >/dev/null 2>&1 && DFS=$(char_guid dfsdfsd)
[ -z "$DFS" ] && { echo "[orch] no dfsdfsd character" >&2; exit 1; }

# Geometry: observer starts at pad O, loops E/N/W/S around it and back; peer (mover) sits at pad N,
# a few yards from O — close enough to stay inside the box across the WHOLE loop (the loop's
# farthest point from O is ~85yd on the diagonal; the AOI box's guaranteed-visible radius is
# 100yd), so the peer legitimately never leaves the observer's view at any point along it.
OX=-8940; OY=-350; OZ=82
FX=-8940; FY=-750; FZ=82  # "far" spot: outside the box, for the login precondition
NX=-8935; NY=-345; NZ=82  # "near" spot: ~7yd from O, inside the box for the whole loop

CMD_OBS=/tmp/ws_aoimc_cmd_$$; ACK_OBS=/tmp/ws_aoimc_ack_$$; CMD_MOV=/tmp/ws_aoimc_mover_cmd_$$
OBS_READY=/tmp/ws_aoimc_obs_ready_$$; MOV_READY=/tmp/ws_aoimc_mover_ready_$$
rm -f "$CMD_OBS" "$ACK_OBS" "$CMD_MOV" "$OBS_READY" "$MOV_READY"

stay_start TEST Ginger || exit 1
scall debug_teleport "$GINGER" 0 $OX $OY $OZ 0
stay_stop
stay_start TEST2 dfsdfsd || exit 1
scall debug_teleport "$DFS" 0 $FX $FY $FZ 0
stay_stop

# mover first (so the observer's login precondition sees it already in-world but FAR)
timeout 240 "$WC" TEST2 dfsdfsd aoi-mover "$CMD_MOV" "$MOV_READY" >/tmp/ws_aoimc_mover.log 2>&1 &
MOVER=$!
wait_for_file 20 "$MOV_READY" || { echo "[orch] mover never ready" >&2; kill $MOVER 2>/dev/null; exit 1; }
rm -f "$MOV_READY"

timeout 240 "$WC" TEST Ginger aoi-observer "$DFS" "$CMD_OBS" "$ACK_OBS" "$OBS_READY" >/tmp/ws_aoimc_observer.log 2>&1 &
OBS=$!
wait_for_file 20 "$OBS_READY" || { echo "[orch] observer never ready (peer visible at login?)" >&2; cat /tmp/ws_aoimc_observer.log; kill $OBS $MOVER 2>/dev/null; exit 1; }
rm -f "$OBS_READY"
step_ok "login precondition: peer outside AOI not visible"

obs_cmd() { # $1=command -> waits for the observer's ack, asserts OK
  local ack; ack=$(obs_cmd_send "$1" "$CMD_OBS" "$ACK_OBS" 60)
  if grep -q "^OK" <<<"$ack"; then step_ok "observer: $1"; else step_fail "observer: $1 -> ${ack:-no ack}"; fi
}

# bring the peer into view near the observer, and confirm relay works BEFORE the loop (baseline).
obs_cmd expect-create & CMDPID=$!
sleep 1
scall debug_teleport "$DFS" 0 $NX $NY $NZ 0
wait $CMDPID

obs_cmd expect-move & CMDPID=$!
sleep 1
echo "burst $NX $NY $NZ" > "$CMD_MOV"
wait $CMDPID

# THE multi-crossing loop: observer walks E, N, W, S (4 crossings, 4 different directions) back to
# its start. "walk-multi" itself asserts zero create/destroy for the peer DURING the loop (fails
# the observer process, caught below as a non-OK ack / non-zero exit).
obs_cmd "walk-multi $OX $OY $OZ"

# AFTER the loop: the mover's heartbeats must still relay to the observer — the #109-class check,
# now exercised across several crossings/directions instead of a single one.
obs_cmd expect-move & CMDPID=$!
sleep 1
echo "burst $NX $NY $NZ" > "$CMD_MOV"
wait $CMDPID

echo "exit" > "$CMD_MOV"
wait "$MOVER" 2>/dev/null

echo "done" > "$CMD_OBS"
wait "$OBS"; OBS_RC=$?
[ $OBS_RC -ne 0 ] && { echo "[orch] observer exited rc=$OBS_RC"; tail -20 /tmp/ws_aoimc_observer.log; FAILED=1; }

rm -f "$CMD_OBS" "$ACK_OBS" "$CMD_MOV" "$OBS_READY" "$MOV_READY"
if [ "${FAILED:-0}" -eq 0 ]; then echo "[aoi-recenter-multicross] PASS"; exit 0; else echo "[aoi-recenter-multicross] FAIL"; exit 1; fi
