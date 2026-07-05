#!/usr/bin/env bash
# Multi-client relay/AOI regression (work-item 141a): two concurrent wire sessions assert
#   1. peer NOT visible at login when outside the AOI box (observer login precondition),
#   2. peer CREATE_OBJECT on approach (teleport into the box),
#   3. move relay: the peer's MSG_MOVE_HEARTBEATs arrive at the observer (packed-guid matched) —
#      and BOTH directions via the jump-relay pair at the end,
#   4. DESTROY on AOI leave (teleport out of the box),
#   5. CREATE on re-enter, then DESTROY on peer DISCONNECT (+ the entity row despawn via sql).
# Requires the gateway running with GW_AOI=1 (grid-scoped entity subscriptions).
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight aoi-relay
DFS=$(char_guid dfsdfsd)
[ -z "$DFS" ] && timeout 60 "$WC" TEST2 test123 dfsdfsd logout >/dev/null 2>&1 && DFS=$(char_guid dfsdfsd)
[ -z "$DFS" ] && { echo "[orch] no dfsdfsd character" >&2; exit 1; }

# Geometry: observer at pad O; peer starts ~400yd away (far outside the 125yd AOI box).
OX=-8940; OY=-350; OZ=82
FX=-8940; FY=-750; FZ=82
NX=-8935; NY=-345; NZ=82   # "near" spot: ~7yd from the observer

# Run-scoped handshake paths (work-item 161): defined ONCE here, passed as wire-client args.
CMD_OBS=/tmp/ws_aoi_cmd_$$; ACK_OBS=/tmp/ws_aoi_ack_$$; CMD_MOV=/tmp/ws_aoi_mover_cmd_$$
OBS_READY=/tmp/ws_aoi_obs_ready_$$; MOV_READY=/tmp/ws_aoi_mover_ready_$$
RELAY_READY=/tmp/wc_relay_ready_$$
rm -f "$CMD_OBS" "$ACK_OBS" "$CMD_MOV" "$OBS_READY" "$MOV_READY" "$RELAY_READY"

# stage stored positions (login places each session at its stored character position)
stay_start TEST test123 Ginger || exit 1
scall debug_teleport "$GINGER" 0 $OX $OY $OZ 0
stay_stop
stay_start TEST2 test123 dfsdfsd || exit 1
scall debug_teleport "$DFS" 0 $FX $FY $FZ 0
stay_stop

# mover first (so the observer's login precondition sees it already in-world but FAR)
timeout 240 "$WC" TEST2 test123 dfsdfsd aoi-mover "$CMD_MOV" "$MOV_READY" >/tmp/ws_aoi_mover.log 2>&1 &
MOVER=$!
wait_for_file 20 "$MOV_READY" || { echo "[orch] mover never ready" >&2; kill $MOVER 2>/dev/null; exit 1; }
rm -f "$MOV_READY"

timeout 240 "$WC" TEST test123 Ginger aoi-observer "$DFS" "$CMD_OBS" "$ACK_OBS" "$OBS_READY" >/tmp/ws_aoi_observer.log 2>&1 &
OBS=$!
wait_for_file 20 "$OBS_READY" || { echo "[orch] observer never ready (peer visible at login?)" >&2; cat /tmp/ws_aoi_observer.log; kill $OBS $MOVER 2>/dev/null; exit 1; }
rm -f "$OBS_READY"
step_ok "login precondition: peer outside AOI not visible"

obs_cmd() { # $1=command -> waits for the observer's ack (obs_cmd_send mechanics), asserts OK
  local ack; ack=$(obs_cmd_send "$1" "$CMD_OBS" "$ACK_OBS" 35)
  if grep -q "^OK" <<<"$ack"; then step_ok "observer: $1"; else step_fail "observer: $1 -> ${ack:-no ack}"; fi
}

# 2. approach -> CREATE
echo "burst-noop" >/dev/null # (placeholder: teleports drive the row change)
obs_cmd expect-create & CMDPID=$!
sleep 1
scall debug_teleport "$DFS" 0 $NX $NY $NZ 0
wait $CMDPID

# 3. move relay peer->observer (real MSG_MOVE_HEARTBEATs through the mover's socket)
obs_cmd expect-move & CMDPID=$!
sleep 1
echo "burst $NX $NY $NZ" > "$CMD_MOV"
wait $CMDPID

# 4. peer DISCONNECT -> DESTROY + row despawn (the peer is in-scope, so the entity delete relays)
obs_cmd expect-destroy & CMDPID=$!
sleep 1
echo "exit" > "$CMD_MOV"
wait $CMDPID
wait "$MOVER" 2>/dev/null
sleep 2
assert_eq "disconnect: peer entity row despawned" "$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE guid = $DFS")" "0"

# 5. AOI LEAVE + RE-ENTER boundary: reconnect the peer in-box (fresh entity insert -> CREATE),
# teleport it out of the box -> DESTROY, then (5b) teleport the SAME live session back in -> CREATE
# again. The re-enter half is work-item 144: the gateway can receive the return as an UPDATE of a
# still-cached row (no on_insert), so the offer-on-update path must deliver the CREATE — no relog.
sleep 3 # settle: the abrupt disconnect's cleanup must finish before the same account relogs
# The abrupt disconnect does NOT persist the live position — stage the stored row directly so the
# reconnect materializes INSIDE the observer's box (a genuine fresh-insert CREATE).
sqlq "UPDATE game_character SET x = $NX, y = $NY, z = $NZ WHERE guid = $DFS" >/dev/null
rm -f "$MOV_READY"
obs_cmd expect-create & CMDPID=$!
sleep 1 # let the observer arm (clear the stale sighting) BEFORE the login CREATE can arrive
timeout 120 "$WC" TEST2 test123 dfsdfsd aoi-mover "$CMD_MOV" "$MOV_READY" >/tmp/ws_aoi_mover2.log 2>&1 &
MOVER=$!
wait_for_file 20 "$MOV_READY"
rm -f "$MOV_READY"
wait $CMDPID

obs_cmd expect-destroy & CMDPID=$!
sleep 1
scall debug_teleport "$DFS" 0 $FX $FY $FZ 0
wait $CMDPID

# 5b. re-enter with the SAME live peer session (work-item 144's done-when assertion).
obs_cmd expect-create & CMDPID=$!
sleep 1
scall debug_teleport "$DFS" 0 $NX $NY $NZ 0
wait $CMDPID

echo "exit" > "$CMD_MOV"
wait "$MOVER" 2>/dev/null

echo "done" > "$CMD_OBS"
wait "$OBS"; OBS_RC=$?
[ $OBS_RC -ne 0 ] && { echo "[orch] observer exited rc=$OBS_RC"; tail -5 /tmp/ws_aoi_observer.log; FAILED=1; }
sleep 4 # settle before the relay pairs relog the same two accounts

# 3b. move relay BOTH WAYS: the existing jump-relay pair, run in each direction. The relay-sender
# mode sends its jump with the CANONICAL test position baked into the packet (-8968,-129 — the
# module persists + scopes recipients by it), so stage BOTH stored rows at the canonical move-relay
# geometry (~32yd apart, one grid box) exactly like scenario-lib's position_apart fixture.
sqlq "UPDATE game_character SET x = -8968.0, y = -129.0, z = 83.4 WHERE guid = $GINGER" >/dev/null
sqlq "UPDATE game_character SET x = -8945.0, y = -107.0, z = 83.4 WHERE guid = $DFS" >/dev/null
echo "[orch] move relay A->B (Ginger jumps, dfsdfsd observes)…"
rm -f "$RELAY_READY"
"$WC" TEST2 test123 dfsdfsd relay-observer "$RELAY_READY" >/tmp/wc_relay_obs.log 2>&1 &
ORC=$!
"$WC" TEST test123 Ginger relay-sender "$RELAY_READY" >/tmp/wc_relay_snd.log 2>&1 &
SRC=$!
wait $ORC; RC1=$?
wait $SRC 2>/dev/null
if [ $RC1 -eq 0 ]; then step_ok "move relay A->B (jump received by observer)"; else step_fail "move relay A->B (rc=$RC1)"; fi
echo "[orch] move relay B->A (dfsdfsd jumps, Ginger observes)…"
sleep 4 # settle: both accounts just disconnected from the A->B pair
rm -f "$RELAY_READY"
"$WC" TEST test123 Ginger relay-observer "$RELAY_READY" >/tmp/wc_relay_obs2.log 2>&1 &
ORC=$!
"$WC" TEST2 test123 dfsdfsd relay-sender "$RELAY_READY" >/tmp/wc_relay_snd2.log 2>&1 &
SRC=$!
wait $ORC; RC2=$?
wait $SRC 2>/dev/null
if [ $RC2 -eq 0 ]; then step_ok "move relay B->A (jump received by observer)"; else step_fail "move relay B->A (rc=$RC2)"; fi

rm -f "$CMD_OBS" "$ACK_OBS" "$CMD_MOV" "$OBS_READY" "$MOV_READY" "$RELAY_READY"
if [ "$FAILED" -eq 0 ]; then echo "[aoi-relay] PASS"; exit 0; else echo "[aoi-relay] FAIL"; exit 1; fi
