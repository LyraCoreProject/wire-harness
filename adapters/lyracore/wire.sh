#!/usr/bin/env bash
# THE ADAPTER SEAM (#244).
#
# `vanilla-wire` is a standalone build-5875 client: it has no default account, no default
# password, no default character and no idea what a "LyraCore" is. Everything project-specific —
# which accounts the local dev stack provisions, what their passwords are, which class the
# suite's fixture characters are, where the gateway listens — lives HERE, in one file, and
# reaches the client only through its documented CLI.
#
# Every orchestrator in adapters/lyracore/*.sh calls the client through this wrapper (as $WC,
# defined by scenario-lib.sh) instead of exec'ing the binary directly. That is what keeps the
# client extractable: point $WIRE_BIN at a downloaded wire-harness release and the whole suite
# runs against it unchanged (#245/#246).
#
# USAGE (a drop-in for the old positional form, minus the plaintext password):
#   wire.sh <account> <character> [scenario [args…]]
#   wire.sh <account> <character>                      → the generic login smoke
#
# CONFIGURATION (all optional):
#   WIRE_BIN                path to the vanilla-wire binary   [<repo>/target/debug/vanilla-wire]
#   WIRE_HOST               server host                       [127.0.0.1]
#   WIRE_LOGON_PORT         logon tier port                   [3724]
#   WIRE_WORLD_PORT         world tier port override          [unset → realm-list answer]
#   WIRE_CLASS              class for characters this creates [warlock — the suite's historical
#                           default, from when it was hardcoded in the client]
#   WIRE_PASSWORD_<ACCOUNT> that account's password           [see fixture_password below]
#   WIRE_FIXTURE_PASSWORD   password for the TEST* accounts    [test123]
#   WIRE_SEAM_PASSWORD      password for the SEAMTEST* accounts [seamtest123]
set -u

here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo=$(cd "$here/../.." && pwd)
BIN=${WIRE_BIN:-$repo/target/debug/vanilla-wire}

# The dev stack's fixture credentials. These are LOCAL TEST accounts on a local server; they are
# defaults of this adapter, never of the client — that distinction is the whole point of #244.
# Override any single account with WIRE_PASSWORD_<ACCOUNT> to run against a different stack.
fixture_password() { # $1=account
  local var="WIRE_PASSWORD_$1"
  if [ -n "${!var:-}" ]; then printf '%s' "${!var}"; return; fi
  case "$1" in
    SEAMTEST*) printf '%s' "${WIRE_SEAM_PASSWORD:-seamtest123}" ;;
    *)         printf '%s' "${WIRE_FIXTURE_PASSWORD:-test123}" ;;
  esac
}

[ $# -ge 2 ] || { echo "usage: wire.sh <account> <character> [scenario [args…]]" >&2; exit 2; }
account=$1; character=$2; shift 2

# Scenarios that open a SECOND session name the peer's ACCOUNT as their first argument; the
# client reads that session's password from stdin line 2, so it never appears on a command line.
peer_accounts=()
case "${1:-}" in
  say-range|ignore-whisper) peer_accounts=("${2:-}") ;;
esac

cmd=("$BIN")
if [ $# -eq 0 ]; then
  cmd+=(smoke)
else
  cmd+=(scenario "$@")
fi
cmd+=(--account "$account" --character "$character" --password-stdin)
cmd+=(--host "${WIRE_HOST:-127.0.0.1}" --logon-port "${WIRE_LOGON_PORT:-3724}")
[ -n "${WIRE_WORLD_PORT:-}" ] && cmd+=(--world-port "$WIRE_WORLD_PORT")
cmd+=(--class "${WIRE_CLASS:-warlock}")

# EXEC, and feed stdin by process substitution rather than a pipeline: the orchestrators background
# these ($!), `wait` on them, `timeout` them and (scenario-lib's _stay_wait_exit) identify them by
# `ps -o comm=`. A pipeline would make this wrapper the process they see and the client a child of
# it, so every one of those would target the wrong process. exec keeps the pid, and the process
# name stays `vanilla-wire`.
exec "${cmd[@]}" < <(
  printf '%s\n' "$(fixture_password "$account")"
  for p in ${peer_accounts[@]+"${peer_accounts[@]}"}; do printf '%s\n' "$(fixture_password "$p")"; done
)
