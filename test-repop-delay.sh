#!/usr/bin/env bash
# SMSG_CORPSE_RECLAIM_DELAY wire test.
# After CMSG_REPOP_REQUEST the gateway must emit SMSG_CORPSE_RECLAIM_DELAY(30000ms).
# Usage: tools/wire-client/test-repop-delay.sh
set -uo pipefail
cd "$(dirname "$0")/../.."
CHAR="Ginger"; DB=spacetime-core
CGUID=$(spacetime sql "$DB" "SELECT guid FROM game_world_entity WHERE display_name='$CHAR'" 2>&1 | grep -oE '[0-9]{5,}' | head -1)
[ -z "$CGUID" ] && { echo "[test] character '$CHAR' not found in game_world_entity" >&2; exit 1; }
echo "[test] $CHAR guid=$CGUID"
cargo build -q -p wire-client || exit 1
rm -f /tmp/wc_repop_ready

orchestrate() {
  # Wait until wire-client is in-world and ready
  for _ in $(seq 1 30); do [ -f /tmp/wc_repop_ready ] && break; sleep 1; done
  if [ ! -f /tmp/wc_repop_ready ]; then echo "[orch] timed out waiting for wire-client ready" >&2; exit 1; fi
  spacetime call "$DB" debug_set_health "$CGUID" 0 >/dev/null 2>&1
  echo "[orch] killed $CHAR (guid $CGUID)" >&2
  rm -f /tmp/wc_repop_ready   # signal wire-client to proceed
}
orchestrate &
ORCH=$!
timeout 60 cargo run -q -p wire-client -- TEST test123 "$CHAR" repop "$CGUID"
RC=$?
wait "$ORCH" 2>/dev/null || true
rm -f /tmp/wc_repop_ready
exit $RC
