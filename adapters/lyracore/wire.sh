#!/usr/bin/env bash
set -euo pipefail

# shellcheck source=adapters/lyracore/adapter-env.sh
source "$(dirname "${BASH_SOURCE[0]}")/adapter-env.sh"

if [ "$#" -lt 2 ]; then
  echo "usage: wire.sh <account> <character> [scenario [args...]]" >&2
  exit 2
fi
account=$1
character=$2
shift 2
if [[ ! "$account" =~ ^[A-Za-z0-9_]+$ ]]; then
  echo "[adapter] account must contain only letters, digits and underscores" >&2
  exit 2
fi
wire_build
cmd=("$WIRE_BIN")
if [ "$#" -eq 0 ]; then
  cmd+=(smoke)
else
  cmd+=(scenario "$@")
fi
cmd+=(--account "$account" --character "$character" --password-stdin)
cmd+=(--host "${WIRE_HOST:-127.0.0.1}" --logon-port "${WIRE_LOGON_PORT:-3724}")
if [ -n "${WIRE_WORLD_PORT:-}" ]; then
  cmd+=(--world-port "$WIRE_WORLD_PORT")
fi
cmd+=(--class "${WIRE_CLASS:-warrior}" --race "${WIRE_RACE:-human}")

password_var="WIRE_PASSWORD_$(printf '%s' "$account" | tr '[:lower:]' '[:upper:]')"
if [ -n "${!password_var:-${WIRE_FIXTURE_PASSWORD:-}}" ]; then
  exec "${cmd[@]}" < <(printf '%s\n' "${!password_var:-$WIRE_FIXTURE_PASSWORD}")
fi
exec "${cmd[@]}"
