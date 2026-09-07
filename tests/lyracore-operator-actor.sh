#!/usr/bin/env bash
# Check Operator argument encoding at the CLI boundary without starting a Realm.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT
export LYRACORE_DIR="$test_dir/core with spaces"
mkdir -p "$LYRACORE_DIR/module/src"
source "$root/adapters/lyracore/scenario-lib.sh"
guid=9007199254740993

spacetime() {
  if [ "$1" = call ]; then
    printf '%s\n' "$@" >> "$test_dir/calls"
    return "${CALL_RESULT:-0}"
  elif [[ "$*" == *'SELECT COUNT(*) AS n FROM game_group_member'* ]]; then
    printf ' n\n---\n %s\n' "${MEMBERS:-1}"
  elif [[ "$*" == *'DELETE FROM game_group_member'* ]]; then
    touch "$test_dir/mirror-changed"
  fi
}

check_cleanup() {
  local expected=$1
  : > "$test_dir/calls"
  leave_any_group "$guid"
  [ "$(sed -n '6p' "$test_dir/calls")" = "$expected" ]
  [ "$(wc -l < "$test_dir/calls")" -eq 9 ]
}

check_transfer_release() {
  local expected=$1 expected_count=$2
  : > "$test_dir/calls"
  release_operator_transfer destination 123 "$guid"
  [ "$(sed -n '2p' "$test_dir/calls")" = destination ]
  [ "$(sed -n '4p' "$test_dir/calls")" = release_transfer ]
  [ "$(sed -n '5p' "$test_dir/calls")" = 123 ]
  [ "$(sed -n '6p' "$test_dir/calls")" = "$expected" ]
  [ "$(wc -l < "$test_dir/calls")" -eq "$expected_count" ]
}

check_cleanup "$guid"
check_transfer_release '' 5
touch "$LYRACORE_DIR/module/src/account_ownership.rs"
check_cleanup '{"guid":9007199254740993,"ownership":null}'
check_transfer_release '{"guid":9007199254740993,"ownership":null}' 6

MEMBERS=0
: > "$test_dir/calls"
leave_any_group "$guid"
[ ! -s "$test_dir/calls" ]
unset MEMBERS

rm -f "$test_dir/mirror-changed"
CALL_RESULT=1
if leave_any_group "$guid"; then
  echo 'refused ownership unexpectedly continued cleanup' >&2
  exit 1
fi
[ ! -e "$test_dir/mirror-changed" ]
unset CALL_RESULT

: > "$test_dir/calls"
for invalid_guid in '001' '1","ownership":{}'; do
  if leave_any_group "$invalid_guid"; then
    echo 'invalid guid unexpectedly reached cleanup' >&2
    exit 1
  fi
done
[ ! -s "$test_dir/calls" ]
echo 'PASS: Operator cleanup preserves old arguments and encodes unowned SessionActor precisely'
