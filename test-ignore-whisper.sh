#!/usr/bin/env bash
# Ignore-list whisper-gate probe (moved out of wire-suite.sh's t_ignore_whisper, work-item 162):
# Ginger ignores dfsdfsd, then dfsdfsd's whisper must NOT be delivered. The wire mode asserts a
# strict IgnoreAdded, so purge Ginger's contact rows FIRST (repeatability) and leave them purged
# AFTER (the friend probe tolerates Already, but a stale ignore row would silently eat unrelated
# whisper traffic in later tests) — standalone and suite runs now stage/tear down identically.
# Usage: tools/wire-client/test-ignore-whisper.sh
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh

cargo build -q -p wire-client || exit 1
GINGER=$(char_guid Ginger)
[ -z "$GINGER" ] && { echo "[test] character 'Ginger' not found in game_character" >&2; exit 1; }

sqlq "DELETE FROM game_character_contact WHERE owner_guid = $GINGER" >/dev/null
timeout 90 "$WC" TEST test123 Ginger ignore-whisper TEST2 test123 dfsdfsd
RC=$?
sqlq "DELETE FROM game_character_contact WHERE owner_guid = $GINGER" >/dev/null
exit $RC
