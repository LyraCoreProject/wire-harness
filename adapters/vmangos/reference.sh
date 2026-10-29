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
    scenario=${3:-$repo/scenarios/connection_identity.toml}
    count=$(python3 - "$scenario" <<'PY'
import sys
import tomllib
with open(sys.argv[1], 'rb') as source:
    scenario = tomllib.load(source)
print(len(scenario.get('characters', [])) if scenario.get('schema_version') == 4 else 1)
PY
    ) || exit 2
    passwords=("$REFERENCE_ROOT/credentials/player-password")
    peers=()
    for ((peer=2; peer<=count; peer++)); do
      passwords+=("$REFERENCE_ROOT/credentials/peer${peer}-password")
      peers+=("CONFORMANCE${peer}")
    done
    passwords+=("$REFERENCE_ROOT/credentials/gm-password")
    peer_accounts=$(IFS=,; echo "${peers[*]}")
    cat "${passwords[@]}" |
      env WIRE_ADAPTER=vmangos WIRE_PEER_ACCOUNTS="$peer_accounts" "$wire_bin" record "$scenario" \
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
    if ! python3 - <<'PY'
import fcntl
try:
    fcntl.flock(9, fcntl.LOCK_EX | fcntl.LOCK_NB)
except BlockingIOError:
    raise SystemExit(1)
PY
    then
      echo "[reference] Reference Server is in use; another recording or audit holds the lock" >&2
      exit 3
    fi
    wire_bin=${WIRE_BIN:-$repo/target/debug/vanilla-wire}
    [[ -x $wire_bin ]] || { echo "build vanilla-wire or set WIRE_BIN" >&2; exit 2; }
    characters=$(python3 - "$scenario" <<'PY'
import sys
import tomllib
with open(sys.argv[1], 'rb') as source:
    scenario = tomllib.load(source)
if scenario.get('schema_version') == 4:
    for index, character in enumerate(scenario['characters'], 1):
        fixture = character['fixtures'][0]
        if fixture.get('verb') != 'character':
            raise SystemExit('named Character must start with a Character fixture')
        account = 'CONFORMANCE' if index == 1 else f'CONFORMANCE{index}'
        password = 'player' if index == 1 else f'peer{index}'
        print(f"{account}\t{fixture['name']}\t{password}")
else:
    fixtures = scenario.get('fixtures', [])
    if not fixtures or fixtures[0].get('verb') != 'character':
        raise SystemExit('first fixture must be a Character')
    print(f"CONFORMANCE\t{fixtures[0]['name']}\tplayer")
PY
    ) || exit 2
    character_accounts=()
    while IFS= read -r entry; do
      if [[ -n $entry ]]; then character_accounts+=("$entry"); fi
    done <<< "$characters"
    mkdir -p "$REFERENCE_ROOT/recordings"
    scratch=$(mktemp -d "$REFERENCE_ROOT/recordings/checked.XXXXXXXX")
    started=0
    written=0
    finish() {
      status=$?
      trap - EXIT
      if [[ $written == 1 ]]; then
        reset_characters || echo "[reference] could not remove all fixture Characters" >&2
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
      local account=$1 character=$2 password=$3
      local proof_attempt=1 startup_attempt=0
      while true; do
        if "$wire_bin" scenario char-delete --host 127.0.0.1 --logon-port 13724 \
            --world-port 18085 --account "$account" --character "$character" \
            --password-stdin < "$REFERENCE_ROOT/credentials/${password}-password" \
            > "$scratch/reset.log" 2>&1; then
          return 0
        fi
        if [[ $proof_attempt -lt 3 ]] && grep -Eq 'logon proof rejected|server proof mismatch' "$scratch/reset.log"; then
          echo "[reference] SRP proof failed during fixture reset on handshake $proof_attempt/3; retrying" >&2
          ((proof_attempt+=1))
          continue
        fi
        if [[ ${4:-0} == 1 && $startup_attempt -lt 30 ]] && \
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
    reset_characters() {
      local startup=${1:-0} account character password failed=0
      for entry in "${character_accounts[@]}"; do
        IFS=$'\t' read -r account character password <<< "$entry"
        reset_character "$account" "$character" "$password" "$startup" || failed=1
      done
      return "$failed"
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
    if [[ ${#character_accounts[@]} -gt 1 ]]; then
      python3 "$adapter/setup.py" --ensure-peers "$((${#character_accounts[@]} - 1))" || exit 1
    fi
    reset_characters 1 || exit 1
    written=1
    echo "[reference] recording first sample" >&2
    "$adapter/reference.sh" record "$output" "$scenario" || exit 1
    reset_characters || exit 1
    echo "[reference] recording repeat sample" >&2
    "$adapter/reference.sh" record "$scratch/repeat.json" "$scenario" || exit 1
    echo "[reference] comparing repeat with first sample" >&2
    "$wire_bin" compare "$scenario" "$output" "$scratch/repeat.json" > "$scratch/comparison.json" || {
      cat "$scratch/comparison.json" >&2
      echo "[reference] repeat differs under scenario Equivalence Rules" >&2
      exit 1
    }
    echo "[reference] repeat matches; Golden Trace written to $output" >&2
    reset_characters || exit 1
    written=0
    ;;
  *)
    echo "usage: reference.sh {up|down|ps|logs} [COMPOSE_ARGS...] | record OUTPUT [SCENARIO] | record-checked SCENARIO OUTPUT" >&2
    exit 2
    ;;
esac
