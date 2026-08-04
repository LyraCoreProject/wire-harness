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
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR

source "$ADAPTER_DIR/scenario-lib.sh"
CHAR="Ginger"
# Run-scoped handshake path (work-item 161): defined ONCE here, passed to the wire-client as an
# arg — no shared /tmp literal, so concurrent runs can't collide.
DING_READY=/tmp/wc_ding_ready_$$
# ensure_ginger_home (issue #213), not a bare char_guid: Ginger is the shared long-lived fixture and
# is not guaranteed to still be on lyracore (region-boundary logins can transfer her live row
# to another shard; a stale duplicate from an old create-on-miss fallback can also shadow her at
# login) — self-heals both before handing back a guid.
CGUID=$(ensure_ginger_home "$CHAR")
[ -z "$CGUID" ] && { echo "[test] character '$CHAR' not found in game_character" >&2; exit 1; }

wire_build || exit 1

orchestrate() {
    # Wait until the wire-client is in-world and signals readiness (entity live).
    wait_for_file 30 "$DING_READY" || {
        echo "[orch] wire-client never signalled ready" >&2
        return 1
    }
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
timeout 60 "$WC" TEST "$CHAR" ding "$DING_READY"
RC=$?
wait "$ORCH" 2>/dev/null || true
rm -f "$DING_READY"
exit $RC
