#!/usr/bin/env bash
# Ding-to-L10 integration test (headless, no wine client).
#
# Verifies work-item #032: a mid-session level-up to L10 emits
# PLAYER_CHARACTER_POINTS1=1 in the SMSG_UPDATE_OBJECT VALUES packet
# so the talent pane doesn't require a relog.
#
# Prereqs: local STDB node + gateway up, TEST account provisioned, char "Ginger"
# (guid 5, created by test-cast-flow.sh on first run).
# Exits nonzero on assertion failure.
set -uo pipefail
cd "$(dirname "$0")/../.."

CHAR="Ginger"
DB=spacetime-core
# Run-scoped handshake path (work-item 161): defined ONCE here, passed to the wire-client as an
# arg — no shared /tmp literal, so concurrent runs can't collide.
DING_READY=/tmp/wc_ding_ready_$$
CGUID=$(spacetime sql "$DB" "SELECT guid FROM game_character WHERE name = '$CHAR'" 2>&1 | grep -oE '[0-9]+' | tail -1)
[ -z "$CGUID" ] && { echo "[test] character '$CHAR' not found in game_character" >&2; exit 1; }

cargo build -q -p wire-client || exit 1

orchestrate() {
    # Wait until the wire-client is in-world and signals readiness (entity live).
    for _ in $(seq 1 30); do
        [ -f "$DING_READY" ] && break
        sleep 1
    done
    if [ ! -f "$DING_READY" ]; then
        echo "[orch] wire-client never signalled ready" >&2
        return 1
    fi
    rm -f "$DING_READY"
    sleep 1   # let any login burst drain through the wire-client

    # First bring the entity to L9 (entity exists now — login created it).
    # This may emit a "downding" or levelup VALUES but the wire-client ignores L9 packets.
    spacetime call "$DB" debug_set_level "$CGUID" 9 2>/dev/null || true
    echo "[orch] debug_set_level $CGUID 9 fired (staging at L9)" >&2
    sleep 2   # let the L9 packet arrive and be consumed by the wire-client recv_raw loop

    # Now trigger the L10 ding — this is the packet we assert.
    spacetime call "$DB" debug_set_level "$CGUID" 10 2>/dev/null || true
    echo "[orch] debug_set_level $CGUID 10 fired — expecting levelup VALUES packet" >&2
}

rm -f "$DING_READY"
orchestrate &
ORCH=$!
timeout 60 cargo run -q -p wire-client -- TEST test123 "$CHAR" ding "$DING_READY"
RC=$?
wait "$ORCH" 2>/dev/null || true
rm -f "$DING_READY"
exit $RC
