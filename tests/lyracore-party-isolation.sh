#!/usr/bin/env bash
# Check strict SQL reads and the absence of realm-wide party cleanup without starting a Realm.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT
export LYRACORE_DIR="$test_dir/core"
mkdir -p "$LYRACORE_DIR"
source "$root/adapters/lyracore/scenario-lib.sh"

spacetime() {
  case "${SQL_CASE:-value}" in
    value) printf ' n\n---\n 0\n' ;;
    empty) printf ' n\n---\n' ;;
    failure) echo 'database unavailable' >&2; return 7 ;;
  esac
}

[ "$(sql1_required 'SELECT COUNT(*) AS n FROM game_group' fixture)" = 0 ]
SQL_CASE=empty
if sql1_required 'SELECT group_id FROM game_group' fixture >"$test_dir/empty.out" 2>"$test_dir/empty.err"; then
  echo 'empty required SQL result unexpectedly succeeded' >&2
  exit 1
fi
grep -q 'SQL returned no data row' "$test_dir/empty.err"
SQL_CASE=failure
if sql1_required 'SELECT COUNT(*) AS n FROM game_group' fixture >"$test_dir/failure.out" 2>"$test_dir/failure.err"; then
  echo 'failed SQL command unexpectedly succeeded' >&2
  exit 1
fi
grep -q "SQL failed on 'fixture'" "$test_dir/failure.err"
grep -q 'database unavailable' "$test_dir/failure.err"

! grep -R -q 'reset_party_state' "$root/adapters/lyracore"
if grep -n -E 'SELECT (COUNT\(\*\) AS n|leader_guid|loot_method) FROM game_group(_member)?[";]' \
    "$root/adapters/lyracore/test-group.sh" \
    "$root/adapters/lyracore/test-group-loot.sh" \
    "$root/adapters/lyracore/test-party-delete.sh" \
    "$root/adapters/lyracore/test-party-brains.sh" \
    "$root/adapters/lyracore/test-bot-serendipity.sh" \
    "$root/adapters/lyracore/test-bot-follow.sh" \
    "$root/adapters/lyracore/test-bot-invite.sh" \
    "$root/adapters/lyracore/test-class-roles.sh" \
    "$root/adapters/lyracore/test-bot-deadmines.sh"; then
  echo 'party scenario still contains an unscoped group assertion' >&2
  exit 1
fi

echo 'PASS: required SQL reads fail loudly and party scenarios use fixture-owned state'
