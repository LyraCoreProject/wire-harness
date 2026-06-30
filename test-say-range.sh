#!/usr/bin/env bash
# Headless say-range test — verifies that a SAY from Ginger (TEST) is NOT delivered to dfsdfsd
# (TEST2) when they are ~32 yards apart (> the 25yd SAY range gate).
#
# Two wire-clients:
#   speaker  = TEST / Ginger   at (-8968, -129)
#   listener = TEST2 / dfsdfsd at (-8945, -107)   distance ≈ 31.8 yd
#
# Pass criteria (both must hold):
#   a) Speaker receives their OWN SAY (self-echo always delivered).
#   b) Listener does NOT receive the SAY (range-gate silences it).
#
# The test-accounts must exist in the running spacetime-core instance.

set -euo pipefail
cd "$(dirname "$0")/../.."

cargo build -p wire-client -q
./target/debug/wire-client TEST test123 Ginger say-range TEST2 test123 dfsdfsd
