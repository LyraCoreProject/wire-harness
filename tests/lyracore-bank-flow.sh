#!/usr/bin/env bash
# Check that the live bank adapter refuses bad input before it can call remote tools.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
test_dir=$(mktemp -d)
trap 'rm -rf "$test_dir"' EXIT
mkdir -p "$test_dir/bin"
cat > "$test_dir/bin/spacetime" <<'EOF'
#!/usr/bin/env bash
touch "$REMOTE_CALLED"
exit 99
EOF
chmod +x "$test_dir/bin/spacetime"
export PATH="$test_dir/bin:$PATH"
export REMOTE_CALLED="$test_dir/remote-called"

check_refusal() {
  local expected=$1
  shift
  local rc=0
  printf 'secret\n' | bash "$root/adapters/lyracore/test-bank-flow.sh" "$@" \
    >"$test_dir/out" 2>&1 || rc=$?
  [ "$rc" -eq 2 ]
  grep -q "$expected" "$test_dir/out"
  [ ! -e "$REMOTE_CALLED" ]
}

check_refusal usage
check_refusal usage 'bad account' Banktester 12
DB='bad database' check_refusal 'invalid DB name' TEST Banktester 12
DB='--database' check_refusal 'invalid DB name' TEST Banktester 12

rc=0
bash "$root/adapters/lyracore/test-bank-flow.sh" TEST Banktester 12 </dev/null \
  >"$test_dir/out" 2>&1 || rc=$?
[ "$rc" -eq 2 ]
grep -q 'password is required on stdin' "$test_dir/out"
[ ! -e "$REMOTE_CALLED" ]

echo 'PASS: bank adapter refuses invalid input before remote commands'
