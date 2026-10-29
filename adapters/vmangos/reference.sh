#!/usr/bin/env bash
set -euo pipefail
adapter=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd -- "$adapter/../.." && pwd)
export REFERENCE_ROOT="$HOME/.cache/lyracore-scratch/reference-server"
REFERENCE_UID=$(id -u)
REFERENCE_GID=$(id -g)
export REFERENCE_UID REFERENCE_GID
export WIRE_CRASH_DIR="$REFERENCE_ROOT/diagnostics"
case ${1:-} in
  up|down|ps|logs)
    exec docker compose -f "$adapter/compose.yml" "$@"
    ;;
  record)
    [[ $# == 2 || $# == 3 ]] || { echo "usage: reference.sh record OUTPUT [SCENARIO]" >&2; exit 2; }
    wire_bin=${WIRE_BIN:-$repo/target/debug/vanilla-wire}
    [[ -x $wire_bin ]] || { echo "build vanilla-wire or set WIRE_BIN" >&2; exit 2; }
    export WIRE_GM_ACCOUNT=CONFORMANCEGM WIRE_GM_CHARACTER=Conformsetup
    cat "$REFERENCE_ROOT/credentials/player-password" "$REFERENCE_ROOT/credentials/gm-password" |
      env WIRE_ADAPTER=vmangos "$wire_bin" record "${3:-$repo/scenarios/connection_identity.toml}" \
      "$REFERENCE_ROOT/provenance.json" "$2" \
      --host 127.0.0.1 --logon-port 13724 --world-port 18085 \
      --account CONFORMANCE --password-stdin
    ;;
  record-checked)
    [[ $# == 3 ]] || { echo "usage: reference.sh record-checked SCENARIO OUTPUT" >&2; exit 2; }
    scenario=$2
    output=$3
    [[ -f $scenario && ! -e $output && -d $(dirname -- "$output") ]] || {
      echo "scenario and output directory must exist; output file must be new" >&2
      exit 2
    }
    [[ -f $REFERENCE_ROOT/provenance.json ]] || { echo "Reference Server is not provisioned; run setup.py" >&2; exit 2; }
    exec 9> "$REFERENCE_ROOT/.audit.lock"
    if ! flock -n 9; then
      echo "[reference] Reference Server is in use; another recording or audit holds the lock" >&2
      exit 3
    fi
    wire_bin=${WIRE_BIN:-$repo/target/debug/vanilla-wire}
    [[ -x $wire_bin ]] || { echo "build vanilla-wire or set WIRE_BIN" >&2; exit 2; }
    character=$(python3 - "$scenario" <<'PY'
import sys
import tomllib
with open(sys.argv[1], 'rb') as source:
    scenario = tomllib.load(source)
fixtures = scenario.get('fixtures', [])
if not fixtures or fixtures[0].get('verb') != 'character':
    raise SystemExit('first fixture must be a Character')
print(fixtures[0]['name'])
PY
    ) || exit 2
    mkdir -p "$REFERENCE_ROOT/recordings"
    scratch=$(mktemp -d "$REFERENCE_ROOT/recordings/checked.XXXXXXXX")
    started=0
    written=0
    finish() {
      status=$?
      trap - EXIT
      if [[ $written == 1 ]]; then
        reset_character || echo "[reference] could not remove fixture Character $character" >&2
      fi
      if [[ $started == 1 ]]; then
        echo "[reference] stopping Reference Server started by this command" >&2
        if ! "$adapter/reference.sh" down; then
          echo "[reference] stop failed; run reference.sh down" >&2
          [[ $status != 0 ]] || status=1
        fi
      fi
      if [[ $status != 0 && -e $output ]]; then rm -f -- "$output"; fi
      rm -rf -- "$scratch"
      exit "$status"
    }
    reset_character() {
      local proof_attempt=1 startup_attempt=0
      while true; do
        if "$wire_bin" scenario char-delete --host 127.0.0.1 --logon-port 13724 \
            --world-port 18085 --account CONFORMANCE --character "$character" \
            --password-stdin < "$REFERENCE_ROOT/credentials/player-password" \
            > "$scratch/reset.log" 2>&1; then
          return 0
        fi
        if [[ $proof_attempt -lt 3 ]] && grep -Eq 'logon proof rejected|server proof mismatch' "$scratch/reset.log"; then
          echo "[reference] SRP proof failed during fixture reset on handshake $proof_attempt/3; retrying" >&2
          ((proof_attempt+=1))
          continue
        fi
        if [[ ${1:-0} == 1 && $startup_attempt -lt 30 ]] && \
            grep -Eq 'failed to fill whole buffer|Connection refused|timed out' "$scratch/reset.log"; then
          ((startup_attempt+=1))
          echo "[reference] waiting for Reference Server login ($startup_attempt/30)" >&2
          sleep 1
          continue
        fi
        cat "$scratch/reset.log" >&2
        return 1
      done
    }
    trap finish EXIT
    trap 'exit 129' HUP
    trap 'exit 130' INT
    trap 'exit 143' TERM
    running=$("$adapter/reference.sh" ps --status running --services) || exit 1
    if [[ -z $running ]]; then
      echo "[reference] starting provisioned Reference Server" >&2
      started=1
      "$adapter/reference.sh" up -d --wait --wait-timeout 180 || exit 1
    elif [[ $(printf '%s\n' "$running" | sort) != $(printf 'auth\ndatabase\nworld\n' | sort) ]]; then
      echo "[reference] partial Reference Server is running; inspect reference.sh ps" >&2
      exit 1
    else
      echo "[reference] reusing running Reference Server" >&2
    fi
    for attempt in $(seq 1 120); do
      if (echo > /dev/tcp/127.0.0.1/13724) 2>/dev/null && \
          (echo > /dev/tcp/127.0.0.1/18085) 2>/dev/null; then break; fi
      if [[ $attempt == 120 ]]; then echo "[reference] server ports did not open; inspect reference.sh logs" >&2; exit 1; fi
      sleep 1
    done
    reset_character 1 || exit 1
    written=1
    echo "[reference] recording first sample" >&2
    "$adapter/reference.sh" record "$output" "$scenario" || exit 1
    reset_character || exit 1
    echo "[reference] recording repeat sample" >&2
    "$adapter/reference.sh" record "$scratch/repeat.json" "$scenario" || exit 1
    echo "[reference] comparing repeat with first sample" >&2
    "$wire_bin" compare "$scenario" "$output" "$scratch/repeat.json" > "$scratch/comparison.json" || {
      cat "$scratch/comparison.json" >&2
      echo "[reference] repeat differs under scenario Equivalence Rules" >&2
      exit 1
    }
    echo "[reference] repeat matches; Golden Trace written to $output" >&2
    reset_character || exit 1
    written=0
    ;;
  *)
    echo "usage: reference.sh {up|down|ps|logs} [COMPOSE_ARGS...] | record OUTPUT [SCENARIO] | record-checked SCENARIO OUTPUT" >&2
    exit 2
    ;;
esac
