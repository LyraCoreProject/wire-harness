#!/usr/bin/env bash
# THE TWO ROOTS (#245).
#
# Before the extraction every orchestrator opened with `cd "$(dirname "$0")/../.."`, because the
# harness lived at `tools/wire-client/` inside the LyraCore checkout and two directories up was
# always the server repo. Now the harness is its own repository and there are two roots, not one:
#
#   HARNESS_DIR   this repository — the `vanilla-wire` crate and its target/ directory
#   LYRACORE_DIR  a LyraCore checkout — `scripts/`, `gateway/`, `module/`, the world data
#
# These orchestrators are ADAPTER code: they are the half of the old harness that is not protocol.
# They shell out to `spacetime sql` / `spacetime call` against a running LyraCore stack, they read
# constants out of the gateway's source, and they source `scripts/import-manifest.sh`. None of that
# is generic and none of it belongs in `src/`. Sourcing this file is what replaces the old `cd`:
# it resolves both roots and leaves you cd'd into LYRACORE_DIR, so every repo-relative path in the
# orchestrator below still means what it meant.
#
# EXTERNAL REQUIREMENTS of everything under adapters/lyracore/ (none apply to `cargo test`):
#   * a LyraCore checkout                — set $LYRACORE_DIR, or run from inside one
#   * a running LyraCore stack           — SpacetimeDB node + gateway, per its docs/danger-zones.md §3
#   * the `spacetime` CLI on $PATH       — authenticated against that node
#   * the module published WITH          — `--features=debug_reducers`; the orchestrators drive
#     debug reducers                       debug_* reducers to stage and assert server state
#   * this repository's fixture accounts — TEST*/SEAMTEST*, provisioned on that stack (see wire.sh)
#
# CONFIGURATION:
#   LYRACORE_DIR  path to the LyraCore checkout   [autodetected from $PWD]
#   WIRE_BIN      a prebuilt vanilla-wire binary  [built from this repo on demand]
#   …plus everything wire.sh documents (WIRE_HOST, WIRE_PASSWORD_<ACCOUNT>, …).

ADAPTER_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
HARNESS_DIR=$(cd "$ADAPTER_DIR/../.." && pwd)

# A LyraCore checkout, identified by the things these scripts actually reach for.
_is_lyracore() { [ -d "$1/gateway" ] && [ -d "$1/module" ] && [ -f "$1/Cargo.toml" ]; }

if [ -n "${LYRACORE_DIR:-}" ]; then
  # Explicit wins, but VALIDATE it: a wrong LYRACORE_DIR otherwise surfaces as a pile of
  # "no such file" noise from `scripts/…` twenty lines later instead of one clear error here.
  LYRACORE_DIR=$(cd "$LYRACORE_DIR" 2>/dev/null && pwd) \
    || { echo "[adapter] LYRACORE_DIR=$LYRACORE_DIR does not exist" >&2; exit 2; }
  _is_lyracore "$LYRACORE_DIR" \
    || { echo "[adapter] LYRACORE_DIR=$LYRACORE_DIR is not a LyraCore checkout (no gateway/ + module/)" >&2; exit 2; }
elif _is_lyracore "$PWD"; then
  LYRACORE_DIR=$PWD
else
  echo "[adapter] set LYRACORE_DIR to a LyraCore checkout, or run this from inside one." >&2
  echo "[adapter]   LYRACORE_DIR=~/src/LyraCore bash ${BASH_SOURCE[1]:-$0}" >&2
  exit 2
fi
export LYRACORE_DIR
cd "$LYRACORE_DIR" || exit 2

# Build the client from THIS repository's manifest, not from whatever workspace the cwd lands in
# (the cwd is LyraCore now, and after #246 it has no wire-client member at all). Skipped when the
# caller supplied a binary — that is the pinned-release path #246 uses.
wire_build() {
  [ -n "${WIRE_BIN:-}" ] && return 0
  cargo build -q -p wire-client --manifest-path "$HARNESS_DIR/Cargo.toml" \
    || { echo "[adapter] vanilla-wire build failed" >&2; return 1; }
}
