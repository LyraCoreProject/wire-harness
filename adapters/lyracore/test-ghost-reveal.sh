#!/usr/bin/env bash
# Spirit-healer REVEAL-ON-GHOST wire test. A viewer who dies + repops (ghost) must get the spirit-healer
# entity CREATE'd via the on_update reveal WITHOUT relog. Isolates the on_update path, so run the gateway
# with LYRACORE_AOI UNSET (=0) — under LYRACORE_AOI=1 the grid re-entry would reveal it regardless.
# Usage: adapters/lyracore/test-ghost-reveal.sh
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"
CHAR="Ginger"
# Run-scoped handshake path (work-item 161): defined ONCE here, passed as a wire-client arg.
GHOST_READY=/tmp/wc_ghost_ready_$$
# ensure_ginger_home (issue #213), not a bare char_guid: Ginger is the shared long-lived fixture and
# is not guaranteed to still be on lyracore (region-boundary logins can transfer her live row
# to another shard; a stale duplicate from an old create-on-miss fallback can also shadow her at
# login) — self-heals both before handing back a guid.
CGUID=$(ensure_ginger_home "$CHAR")
[ -z "$CGUID" ] && { echo "[test] character '$CHAR' not found in game_character" >&2; exit 1; }
wire_build || exit 1
rm -f "$GHOST_READY"
HEALER=$(spacetime sql "$DB" "SELECT guid FROM game_world_entity WHERE entry=6491 AND owner_guid=0" 2>&1 | grep -oE '[0-9]{15,}' | head -1)
[ -z "$HEALER" ] && { echo "[test] no spirit healer (6491) spawned" >&2; exit 1; }
echo "[test] spirit healer guid=$HEALER"

orchestrate() {
  wait_for_file 30 "$GHOST_READY"
  spacetime call "$DB" debug_set_health $CGUID 0 >/dev/null 2>&1
  sleep 1
  spacetime call "$DB" debug_repop $CGUID >/dev/null 2>&1
  echo "[orch] killed + repopped $CHAR -> ghost" >&2
}
orchestrate &
ORCH=$!
timeout 90 "$WC" TEST "$CHAR" ghost "$HEALER" "$GHOST_READY"
RC=$?
wait "$ORCH" 2>/dev/null || true
rm -f "$GHOST_READY"
exit $RC
