#!/usr/bin/env bash
# test-move-relay.sh — headless two-client test for MSG_MOVE_* peer relay (work-item 057).
#
# Client A (TEST / Ginger)  sends MSG_MOVE_JUMP.
# Client B (TEST2 / dfsdfsd) observes and asserts it receives MSG_MOVE_JUMP_Server (opcode 0xBB).
#
# Both characters start within 32yd of each other in Northshire — well within the 125yd AOI box.
# Coordination via /tmp/wc_relay_ready: observer writes it when in-world; sender reads it.
#
# Usage: bash tools/wire-client/test-move-relay.sh
# Pass exit code: 0. Fail exit code: non-zero.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WC="$SCRIPT_DIR/../../target/debug/wire-client"

if [[ ! -x "$WC" ]]; then
  echo "[test-move-relay] building wire-client…"
  cargo build -p wire-client --manifest-path "$SCRIPT_DIR/../../Cargo.toml" 2>&1
fi

rm -f /tmp/wc_relay_ready

# --- Client B (observer) — runs in background ---
echo "[test-move-relay] launching observer (TEST2 / dfsdfsd)…"
"$WC" TEST2 test123 dfsdfsd relay-observer >/tmp/wc_relay_observer.log 2>&1 &
OBSERVER_PID=$!

# --- Client A (sender) — wait up to 8s for observer to log in, then send jump ---
echo "[test-move-relay] launching sender (TEST / Ginger)…"
"$WC" TEST test123 Ginger relay-sender >/tmp/wc_relay_sender.log 2>&1 &
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
