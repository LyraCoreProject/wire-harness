#!/usr/bin/env bash
# test-logout.sh — headless verification for work item #077 (combat logout gate)
# Tests:
#   1. Out-of-combat logout: SMSG_LOGOUT_RESPONSE(Success) + SMSG_LOGOUT_COMPLETE + entity removed
#   2. In-combat denial: covered by gateway unit tests (logout_while_in_combat_is_denied)
set -e
cd "$(dirname "$0")/../.."

echo "[077] out-of-combat logout probe…"
./target/debug/wire-client TEST test123 Ginger logout
echo "[077] PASS"
