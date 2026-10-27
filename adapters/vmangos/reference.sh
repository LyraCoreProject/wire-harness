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
    [[ $# == 3 ]] || { echo "usage: reference.sh record FRESH_CHARACTER OUTPUT" >&2; exit 2; }
    wire_bin=${WIRE_BIN:-$repo/target/debug/vanilla-wire}
    [[ -x $wire_bin ]] || { echo "build vanilla-wire or set WIRE_BIN" >&2; exit 2; }
    exec env WIRE_ADAPTER=vmangos "$wire_bin" record "$repo/scenarios/connection_identity.toml" \
      "$REFERENCE_ROOT/provenance.json" "$3" \
      --host 127.0.0.1 --logon-port 13724 --world-port 18085 \
      --account CONFORMANCE --character "$2" --password-stdin < "$REFERENCE_ROOT/credentials/gm-password"
    ;;
  *)
    echo "usage: reference.sh {up|down|ps|logs} [COMPOSE_ARGS...] | record FRESH_CHARACTER OUTPUT" >&2
    exit 2
    ;;
esac
