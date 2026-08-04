#!/usr/bin/env bash
# test-logout.sh — headless verification for work item #077 (combat logout gate)
# Tests:
#   1. Out-of-combat logout: SMSG_LOGOUT_RESPONSE(Success) + SMSG_LOGOUT_COMPLETE + entity removed
#   2. In-combat denial: covered by gateway unit tests (logout_while_in_combat_is_denied)
set -e
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"

# The test asserts the OUT-of-combat path, but Ginger's IN_COMBAT flag lingers ~6s past her last
# combat event — a preceding test/batch that fought near her makes this first-in-suite probe read
# FailureInCombat (seen in-gate 2026-07-16). Settle: clear any residual engagement rows and wait
# out the combat-drop window before probing.
CGUID=$(char_guid Ginger)
[ -n "$CGUID" ] && sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = $CGUID" >/dev/null 2>&1 || true
sleep 7
echo "[077] out-of-combat logout probe…"
"$WC" TEST Ginger logout
echo "[077] PASS"
