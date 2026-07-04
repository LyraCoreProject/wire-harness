#!/usr/bin/env bash
# SMSG_CORPSE_RECLAIM_DELAY wire test.
# After CMSG_REPOP_REQUEST the gateway must emit SMSG_CORPSE_RECLAIM_DELAY(30000ms).
# Usage: tools/wire-client/test-repop-delay.sh
set -uo pipefail
cd "$(dirname "$0")/../.."
CHAR="Ginger"; DB=spacetime-core
# Run-scoped handshake path (work-item 161): defined ONCE here, passed as a wire-client arg.
REPOP_READY=/tmp/wc_repop_ready_$$
CGUID=$(spacetime sql "$DB" "SELECT guid FROM game_character WHERE name = '$CHAR'" 2>&1 | grep -oE '[0-9]+' | tail -1)
[ -z "$CGUID" ] && { echo "[test] character '$CHAR' not found in game_character" >&2; exit 1; }
echo "[test] $CHAR guid=$CGUID"
cargo build -q -p wire-client || exit 1
rm -f "$REPOP_READY"

orchestrate() {
  # Wait until wire-client is in-world and ready
  for _ in $(seq 1 30); do [ -f "$REPOP_READY" ] && break; sleep 1; done
  if [ ! -f "$REPOP_READY" ]; then echo "[orch] timed out waiting for wire-client ready" >&2; exit 1; fi
  spacetime call "$DB" debug_set_health "$CGUID" 0 >/dev/null 2>&1
  echo "[orch] killed $CHAR (guid $CGUID)" >&2
  rm -f "$REPOP_READY"   # signal wire-client to proceed
}
orchestrate &
ORCH=$!
timeout 60 cargo run -q -p wire-client -- TEST test123 "$CHAR" repop "$CGUID" "$REPOP_READY"
RC=$?
wait "$ORCH" 2>/dev/null || true
rm -f "$REPOP_READY"
exit $RC
