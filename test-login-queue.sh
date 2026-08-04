#!/usr/bin/env bash
# test-login-queue.sh — headless correctness check for the #180 login queue.
#
# WRITTEN, NOT RUN as part of any automatic suite: driving a login storm against the live stack is
# an operator-approved live-stack action (CLAUDE.md "Live wire tests are operator-gated in attended
# sessions" — ask first with a one-line justification, or hand the operator this exact command).
#
# What it does: provisions N small synthetic accounts (LQ0..LQ{N-1}), then drives N CONCURRENT
# world logins via the `login-queue` wire-client probe (tools/wire-client/src/modes/probes.rs). It
# asserts every connection eventually reaches AuthOk, and that any connection which saw
# AuthWaitQueue observed a monotonically non-increasing position sequence (never worse, only ever
# toward the front — see the probe's own doc comment for the exact FIFO shape it checks).
#
# THIS SCRIPT DOES NOT (RE)START THE GATEWAY OR SET LYRACORE_MAX_SESSIONS. The queue only engages if the
# gateway currently running was launched with a small LYRACORE_MAX_SESSIONS (< N) — see
# docs/danger-zones.md §3 for the launch recipe. Against an unconfigured gateway this still PASSES
# (every connection just admits immediately, `queue_positions_seen` empty for all of them) — that is
# a legitimate, if less interesting, run: it proves the gate is a true no-op when unarmed.
#
# Usage: bash tools/wire-client/test-login-queue.sh [N] [PASSWORD]
#   N        number of concurrent logins (default 5 — small on purpose; #180's storm-scale
#            validation is the separate 300-login / LYRACORE_MAX_SESSIONS=150 run in the operator's
#            window, driven by scripts/run-capacity-bench.sh per docs/gateway-perf-runbooks.md, not
#            this script — this one is the protocol-correctness check, not the throughput one).
#   PASSWORD default test123, matching every other fixture account in this tree.
#
# Suggested live setup before running this (operator window only):
#   1. Launch the gateway with LYRACORE_MAX_SESSIONS=2 (so N=5 forces 3 of the 5 to queue) — see
#      docs/danger-zones.md §3 for the rest of the launch recipe (LYRACORE_SHARD_MAP etc. still apply).
#   2. Run this script.
#   3. Watch the gateway log for "QUEUESTAT depth=... admitted=... active=..." lines while it runs.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$SCRIPT_DIR/../.."
WC="$REPO_ROOT/target/debug/wire-client"
GW="$REPO_ROOT/target/debug/lyracore-gateway"
PREFIX="LQ"

N="${1:-5}"
PASSWORD="${2:-test123}"

if [[ ! -x "$WC" ]]; then
  echo "[test-login-queue] building wire-client…"
  cargo build -p wire-client --manifest-path "$REPO_ROOT/Cargo.toml" 2>&1
fi
if [[ ! -x "$GW" ]]; then
  echo "[test-login-queue] building gateway (for the 'provision' subcommand)…"
  cargo build -p lyracore-gateway --manifest-path "$REPO_ROOT/Cargo.toml" 2>&1
fi

: "${LYRACORE_COORDINATOR_TOKEN:?set LYRACORE_COORDINATOR_TOKEN (see docs/testing.md §2) before provisioning}"

echo "[test-login-queue] provisioning ${N} accounts ${PREFIX}0..${PREFIX}$((N - 1))…"
for i in $(seq 0 $((N - 1))); do
  printf '%s\n' "$PASSWORD" | "$GW" provision "${PREFIX}${i}" --password-stdin >/dev/null
done

echo "[test-login-queue] driving ${N} concurrent world logins…"
if "$WC" "$PREFIX" "$PASSWORD" _ login-queue "$N"; then
  echo "[test-login-queue] PASS: all ${N} connections reached AuthOk (FIFO shape held for any that queued)"
  exit 0
else
  echo "[test-login-queue] FAIL: see the [wire] FAIL lines above"
  exit 1
fi
