#!/usr/bin/env bash
# test-packet-lint.sh — the outbound packet-lint wall (testing-hardening §3.2). The gateway lints
# every RAW frame against the root-caused 5875 crash classes and logs "packet-lint VIOLATION" on a
# hit. This gate drives a flow that exercises the linted paths (login VALUES partials, a rep-bar
# grant -> SET_FACTION_STANDING relay, a cast) and asserts ZERO new violation lines appeared in the
# gateway log. Marker-count based: the log accumulates across runs, so only the DELTA counts.
set -uo pipefail
cd "$(dirname "$0")/../.."
DB=lyracore
# $WC comes from scenario-lib.sh (the adapters/lyracore/wire.sh seam) — do not re-point it at the binary.
GWLOG=${GWLOG:-/tmp/gw.log}
source tools/wire-client/scenario-lib.sh

GINGER=$(char_guid Ginger)
[ -z "$GINGER" ] && { echo "[packet-lint] no Ginger" >&2; exit 1; }
[ -f "$GWLOG" ] || { echo "[packet-lint] no gateway log at $GWLOG (set GWLOG=)" >&2; exit 1; }

BEFORE=$(grep -c "packet-lint VIOLATION" "$GWLOG" || true)

# Exercise the linted paths: a login (VALUES partials + INITIALIZE_FACTIONS), a rep grant while
# live (the SET_FACTION_STANDING raw relay), and an aura-bearing cast (raw aura VALUES).
S=/tmp/wc_lint_stay_$$
rm -f "$S"
( timeout 30 "$WC" TEST Ginger stay "$S" 25 >/dev/null 2>&1 & )
sleep 5
scall debug_grant_reputation "$GINGER" 50900 10
scall debug_grant_reputation "$GINGER" 50900 -10
sleep 2
echo done > "$S"
sleep 2

AFTER=$(grep -c "packet-lint VIOLATION" "$GWLOG" || true)
DELTA=$(( ${AFTER:-0} - ${BEFORE:-0} ))
if [ "$DELTA" -gt 0 ]; then
  echo "[packet-lint] FAIL: $DELTA new violation(s):" >&2
  grep "packet-lint VIOLATION" "$GWLOG" | tail -n "$DELTA" >&2
  exit 1
fi
echo "[packet-lint] PASS — 0 new violations across login + rep-relay flow"
