#!/usr/bin/env bash
# SMSG_CORPSE_RECLAIM_DELAY wire test.
# After CMSG_REPOP_REQUEST the gateway must emit SMSG_CORPSE_RECLAIM_DELAY(30000ms).
# Usage: tools/wire-client/test-repop-delay.sh
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
CHAR="Ginger"
# Run-scoped handshake path (work-item 161): defined ONCE here, passed as a wire-client arg.
REPOP_READY=/tmp/wc_repop_ready_$$
CGUID=$(char_guid "$CHAR")
[ -z "$CGUID" ] && { echo "[test] character '$CHAR' not found in game_character" >&2; exit 1; }
echo "[test] $CHAR guid=$CGUID"
cargo build -q -p wire-client || exit 1
rm -f "$REPOP_READY"

orchestrate() {
  # Wait until wire-client is in-world and ready
  wait_for_file 30 "$REPOP_READY" || { echo "[orch] timed out waiting for wire-client ready" >&2; exit 1; }
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
# Teardown (moved from wire-suite.sh, work-item 162): the test leaves $CHAR dead/ghost —
# resurrect + heal so whatever runs next (standalone OR suite) doesn't start on a corpse.
if stay_start TEST test123 "$CHAR"; then
  scall debug_spirit_healer_res "$CGUID" || true
  scall debug_set_health "$CGUID" 100000 || true
  stay_stop
fi
exit $RC
