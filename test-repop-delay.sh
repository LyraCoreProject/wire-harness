#!/usr/bin/env bash
# SMSG_CORPSE_RECLAIM_DELAY wire test.
# After CMSG_REPOP_REQUEST the gateway must emit SMSG_CORPSE_RECLAIM_DELAY(30000ms).
# Usage: tools/wire-client/test-repop-delay.sh
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
# Disposable char (2026-07-18): this test asserts the FIRST-death reclaim delay (30s), but shared
# Ginger's death-ladder escalates 30->60->120s across the suite's many death tests (scenario_death,
# repop, ...), so on Ginger it flakes to 120s. A fresh char has NO death history -> death_expire_micros
# 0 -> first death -> deterministic 30s. (§3.4 disposable-char pattern.)
CHAR="Repoptester"
# Run-scoped handshake path (work-item 161): defined ONCE here, passed as a wire-client arg.
REPOP_READY=/tmp/wc_repop_ready_$$
CGUID=$(fresh_char "$CHAR" warrior 5)
[ -z "$CGUID" ] && { echo "[test] fresh_char '$CHAR' failed" >&2; exit 1; }
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
timeout 60 "$WC" TEST "$CHAR" repop "$CGUID" "$REPOP_READY"
RC=$?
wait "$ORCH" 2>/dev/null || true
rm -f "$REPOP_READY"
# Teardown: the disposable char leaves no shared state — just delete it (its death/ghost state
# dies with it). drop_char is delete-first-safe.
drop_char "$CHAR"
exit $RC
