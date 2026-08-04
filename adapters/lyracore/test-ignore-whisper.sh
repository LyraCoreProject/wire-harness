#!/usr/bin/env bash
# Ignore-list whisper-gate probe (moved out of wire-suite.sh's t_ignore_whisper, work-item 162):
# Ginger ignores dfsdfsd, then dfsdfsd's whisper must NOT be delivered. The wire mode asserts a
# strict IgnoreAdded, so purge Ginger's contact rows FIRST (repeatability) and leave them purged
# AFTER (the friend probe tolerates Already, but a stale ignore row would silently eat unrelated
# whisper traffic in later tests) — standalone and suite runs now stage/tear down identically.
# Usage: adapters/lyracore/test-ignore-whisper.sh
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"

wire_build || exit 1
GINGER=$(char_guid Ginger)
[ -z "$GINGER" ] && { echo "[test] character 'Ginger' not found in game_character" >&2; exit 1; }

sqlq "DELETE FROM game_character_contact WHERE owner_guid = $GINGER" >/dev/null
timeout 90 "$WC" TEST Ginger ignore-whisper TEST2 dfsdfsd
RC=$?
sqlq "DELETE FROM game_character_contact WHERE owner_guid = $GINGER" >/dev/null
exit $RC
