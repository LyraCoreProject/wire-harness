#!/usr/bin/env bash
# Check dive adapter input and terrain preflight without starting a Realm.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT
mkdir -p "$test_dir/bin" "$test_dir/core/gateway" "$test_dir/core/module"
touch "$test_dir/core/Cargo.toml"

cat > "$test_dir/bin/spacetime" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$REMOTE_LOG"
if [[ "$*" == *'COUNT(*)'* ]]; then
  printf ' n\n---\n %s\n' "${FAKE_CELL_COUNT:-0}"
elif [[ "$*" == *'SELECT has_liquid'* ]]; then
  printf ' has_liquid\n------------\n %s\n' "${FAKE_HAS_LIQUID:-false}"
elif [[ "$*" == *'SELECT liquid_level'* ]]; then
  printf ' liquid_level\n--------------\n %s\n' "${FAKE_LIQUID_LEVEL:-0}"
fi
EOF
chmod +x "$test_dir/bin/spacetime"
export PATH="$test_dir/bin:$PATH"
export REMOTE_LOG="$test_dir/remote.log"
export LYRACORE_DIR="$test_dir/core"

check_refusal() {
  local expected=$1
  shift
  local rc=0
  : > "$REMOTE_LOG"
  printf 'secret\n' | bash "$root/adapters/lyracore/test-dive-flow.sh" "$@" \
    >"$test_dir/out" 2>&1 || rc=$?
  [ "$rc" -eq 2 ]
  grep -q "$expected" "$test_dir/out"
  [ ! -s "$REMOTE_LOG" ]
}

check_refusal usage
check_refusal usage 'bad account' Diveprobe 0 -9000 -400 20 10
check_refusal usage TEST Sharedname 0 -9000 -400 20 10
check_refusal usage TEST "DiveA'B" 0 -9000 -400 20 10
check_refusal usage TEST Diveprobe 0 NaN -400 20 10
check_refusal usage TEST Diveprobe 0 -9000 -400 10 20
DB='--database' check_refusal 'invalid DB name' TEST Diveprobe 0 -9000 -400 20 10

rc=0
bash "$root/adapters/lyracore/test-dive-flow.sh" TEST Diveprobe 0 -9000 -400 20 10 </dev/null \
  >"$test_dir/out" 2>&1 || rc=$?
[ "$rc" -eq 2 ]
grep -q 'password is required on stdin' "$test_dir/out"
[ ! -s "$REMOTE_LOG" ]

# The cell math is LyraCore's (17066.666 - coord) / (533.3333 / 16). A missing imported cell must
# stop after the read-only preflight and before the client can delete or create a Character.
rc=0
printf 'secret\n' | bash "$root/adapters/lyracore/test-dive-flow.sh" \
  TEST Diveprobe 0 -9000 -400 20 10 >"$test_dir/out" 2>&1 || rc=$?
[ "$rc" -eq 1 ]
grep -q 'cell=(782,524).*missing' "$test_dir/out"
[ "$(wc -l < "$REMOTE_LOG")" -eq 1 ]
grep -q 'SELECT COUNT' "$REMOTE_LOG"

: > "$REMOTE_LOG"
rc=0
FAKE_CELL_COUNT=1 FAKE_HAS_LIQUID=true FAKE_LIQUID_LEVEL=50 \
  bash -c 'printf "secret\n" | bash "$1" TEST Diveprobe 0 -9000 -400 20 10' \
  _ "$root/adapters/lyracore/test-dive-flow.sh" >"$test_dir/out" 2>&1 || rc=$?
[ "$rc" -eq 1 ]
grep -q 'coordinates do not cross imported liquid level' "$test_dir/out"
[ "$(wc -l < "$REMOTE_LOG")" -eq 3 ]
! grep -q 'char-delete\|make-char\|debug_set_level\|UPDATE game_character' "$REMOTE_LOG"

echo 'PASS: dive adapter refuses unsafe input and unsuitable terrain before Character changes'
