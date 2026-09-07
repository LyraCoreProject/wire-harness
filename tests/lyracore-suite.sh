#!/usr/bin/env bash
# Exercise scenario dispatch and accounting without starting a Realm.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
source "$root/adapters/lyracore/wire-suite.sh"
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT
ADAPTER_DIR="$test_dir/adapter with spaces"
LOGDIR="$test_dir/logs"
mkdir -p "$ADAPTER_DIR" "$LOGDIR" "$test_dir/core"
cd "$test_dir/core"

printf 'echo actual-scenario-ran\n' > "$ADAPTER_DIR/test-logout.sh"
run_test logout > "$test_dir/result"
[ "$PASS_N" -eq 1 ]
grep -q '^actual-scenario-ran$' "$LOGDIR/logout.log"

printf 'exit 5\n' > "$ADAPTER_DIR/test-logout.sh"
run_test logout > "$test_dir/result"
[ "$FAIL_N" -eq 1 ]
[ "$SPAWN_N" -eq 0 ]

printf 'echo "SKIP: missing fixture"\nexit 77\n' > "$ADAPTER_DIR/test-logout.sh"
run_test logout > "$test_dir/result"
[ "$SKIP_N" -eq 1 ]
grep -q 'missing fixture' "$test_dir/result"

rm "$ADAPTER_DIR/test-logout.sh"
run_test logout > "$test_dir/result"
[ "$SPAWN_N" -eq 1 ]
[ "$FAIL_N" -eq 1 ]
grep -q 'SPAWN FAILURE' "$test_dir/result"
START=$(date +%s)
rc=0
summarize_results > "$test_dir/summary" || rc=$?
[ "$rc" -eq 2 ]
grep -q '1 startup failures' "$test_dir/summary"
grep -q 'RESULT: ERROR' "$test_dir/summary"
! grep -q 'RESULT: GREEN' "$test_dir/summary"

printf 'exit 126\n' > "$ADAPTER_DIR/test-logout.sh"
run_test logout > "$test_dir/result"
[ "$SPAWN_N" -eq 2 ]

select_tests transfer_crash_matrix who logout who
[ "${SELECTED_TESTS[*]}" = 'logout who transfer_crash_matrix' ]
WS_ONLY='who roll' select_tests
[ "${SELECTED_TESTS[*]}" = 'who roll' ]
WS_ONLY='who' select_tests logout
[ "${SELECTED_TESTS[*]}" = 'logout' ]
select_tests
[ "${SELECTED_TESTS[*]}" = "${ALL_TESTS[*]}" ]

rc=0
bash "$root/adapters/lyracore/wire-suite.sh" nonexistent > "$test_dir/invalid" 2>&1 || rc=$?
[ "$rc" -eq 2 ]
grep -q 'unknown test: nonexistent' "$test_dir/invalid"
! grep -q preflight "$test_dir/invalid"
rc=0
WS_ONLY='  ' bash "$root/adapters/lyracore/wire-suite.sh" > "$test_dir/empty" 2>&1 || rc=$?
[ "$rc" -eq 2 ]
grep -q 'no tests selected' "$test_dir/empty"

echo 'PASS: scenario roots, result accounting, subset selection and early refusal'
