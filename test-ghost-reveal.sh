#!/usr/bin/env bash
# Spirit-healer REVEAL-ON-GHOST wire test. A viewer who dies + repops (ghost) must get the spirit-healer
# entity CREATE'd via the on_update reveal WITHOUT relog. Isolates the on_update path, so run the gateway
# with GW_AOI UNSET (=0) — under GW_AOI=1 the grid re-entry would reveal it regardless.
# Usage: tools/wire-client/test-ghost-reveal.sh
set -uo pipefail
cd "$(dirname "$0")/../.."
CHAR="Ginger"; CGUID=5; DB=spacetime-core
cargo build -q -p wire-client || exit 1
rm -f /tmp/wc_ghost_ready
HEALER=$(spacetime sql "$DB" "SELECT guid FROM game_world_entity WHERE entry=6491 AND owner_guid=0" 2>&1 | grep -oE '[0-9]{15,}' | head -1)
[ -z "$HEALER" ] && { echo "[test] no spirit healer (6491) spawned" >&2; exit 1; }
echo "[test] spirit healer guid=$HEALER"

orchestrate() {
  for _ in $(seq 1 30); do [ -f /tmp/wc_ghost_ready ] && break; sleep 1; done
  spacetime call "$DB" debug_set_health $CGUID 0 >/dev/null 2>&1
  sleep 1
  spacetime call "$DB" debug_repop $CGUID >/dev/null 2>&1
  echo "[orch] killed + repopped $CHAR -> ghost" >&2
}
orchestrate &
ORCH=$!
timeout 90 cargo run -q -p wire-client -- TEST test123 "$CHAR" ghost "$HEALER"
RC=$?
wait "$ORCH" 2>/dev/null || true
rm -f /tmp/wc_ghost_ready
exit $RC
