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
# The test-accounts must exist in the running lyracore instance.

set -euo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"

wire_build || exit 1
# Staging (moved from wire-suite.sh, work-item 162): park the two characters ~32yd apart so
# standalone runs get the same geometry the suite used to set up in its t_say_range wrapper.
position_apart || { echo "[test-say-range] staging failed (position_apart)" >&2; exit 1; }
"$WC" TEST Ginger say-range TEST2 dfsdfsd
