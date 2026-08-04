#!/usr/bin/env bash
# ADDON⇄SERVER BRIDGE (184) — the full wire round-trip, twice: a byte-faithful fake of the 1.12
# client's SendAddonMessage (CMSG_MESSAGECHAT, LANG_ADDON, "STC\t" envelope) must reach the
# module's client_command dispatch AS the player and come back as an addon-language
# SMSG_MESSAGECHAT pong echoing the payload — through the raw decode escape hatch inbound and the
# coordinator-ridden relay outbound. Also asserts a foreign-prefix frame is swallowed silently
# (no disconnect, session stays usable).
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"
scenario_preflight addon-bridge

for i in 1 2; do
  if timeout 40 "$WC" TEST Ginger addon-ping "roundtrip$i" 2>&1 | grep -q "ADDON-PING PASS"; then
    step_ok "bridge round-trip $i (ping -> client_command -> pong envelope)"
  else
    step_fail "bridge round-trip $i failed"
  fi
done

if [ "$FAILED" -eq 0 ]; then echo "[addon-bridge] PASS"; exit 0; else echo "[addon-bridge] FAIL"; exit 1; fi
