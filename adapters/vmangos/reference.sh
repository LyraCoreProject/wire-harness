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
  *)
    echo "usage: reference.sh {up|down|ps|logs} [COMPOSE_ARGS...] | record OUTPUT [SCENARIO]" >&2
    exit 2
    ;;
esac
