#!/usr/bin/env bash
# test-move-relay.sh — headless two-client test for MSG_MOVE_* peer relay (work-item 057).
#
# Client A (TEST / Ginger)  sends MSG_MOVE_JUMP.
# Client B (TEST2 / dfsdfsd) observes and asserts it receives MSG_MOVE_JUMP_Server (opcode 0xBB).
#
# Both characters are staged within 32yd of each other in Northshire (position_apart) — well
# within the 125yd AOI box. Coordination via the run-scoped $RELAY_READY file (work-item 161),
# passed to both wire-client sides as an arg: observer writes it when in-world; sender polls the
# same path.
#
# Usage: bash adapters/lyracore/test-move-relay.sh
# Pass exit code: 0. Fail exit code: non-zero.

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"

# $WC is the adapter wrapper (always present), so the old `[ -x "$WC" ]` guard could no longer
# tell whether the CLIENT had been built — build it the way every other orchestrator does.
wire_build || exit 1

# Staging (moved from wire-suite.sh, work-item 162): park the two characters ~32yd apart so
# standalone runs get the same geometry the suite used to set up in its t_move_relay wrapper.
position_apart || { echo "[test-move-relay] staging failed (position_apart)" >&2; exit 1; }

# Run-scoped handshake path (work-item 161): defined ONCE here, passed to both sides as an arg.
RELAY_READY=/tmp/wc_relay_ready_$$
rm -f "$RELAY_READY"

# --- Client B (observer) — runs in background ---
echo "[test-move-relay] launching observer (TEST2 / dfsdfsd)…"
"$WC" TEST2 dfsdfsd relay-observer "$RELAY_READY" >/tmp/wc_relay_observer.log 2>&1 &
OBSERVER_PID=$!

# --- Client A (sender) — wait up to 8s for observer to log in, then send jump ---
echo "[test-move-relay] launching sender (TEST / Ginger)…"
"$WC" TEST Ginger relay-sender "$RELAY_READY" >/tmp/wc_relay_sender.log 2>&1 &
SENDER_PID=$!

# --- Wait for both to finish ---
SENDER_RC=0
OBSERVER_RC=0
wait "$SENDER_PID"  || SENDER_RC=$?
wait "$OBSERVER_PID" || OBSERVER_RC=$?

echo "=== SENDER LOG ==="
cat /tmp/wc_relay_sender.log
echo "=== OBSERVER LOG ==="
cat /tmp/wc_relay_observer.log

if [[ "$OBSERVER_RC" -eq 0 ]]; then
  echo "[test-move-relay] PASS: observer received MSG_MOVE_JUMP_Server relay from sender"
  exit 0
else
  echo "[test-move-relay] FAIL: observer exit=$OBSERVER_RC sender exit=$SENDER_RC"
  exit 1
fi
