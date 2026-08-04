#!/usr/bin/env bash
# Random-walk soak harness (work-item 141b): SOAK_SECS (default 600 = 10 min) of continuous
# movement heartbeats + periodic self-casts + engage/disengage cycles against live creatures,
# while watching /tmp/gw.log for gateway decode/desync errors and the module log for panics.
# Exit nonzero on any error growth; prints the run summary (actions sent, packets seen, errors).
# Usage: SOAK_SECS=600 bash tools/wire-client/test-soak.sh
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight soak

SOAK_SECS="${SOAK_SECS:-600}"
PAD_X=-8920; PAD_Y=-180; PAD_Z=82
LYRACORE_LOG=/tmp/gw.log

gw_errs()  { grep -ciE "decode|desync|ERROR" "$LYRACORE_LOG" 2>/dev/null || echo 0; }
mod_panics() { spacetime logs "$DB" 2>/dev/null | grep -c "panicked" || echo 0; }

# stage: full vitals + mana for the periodic casts, a couple of wolves for ambient combat, and the
# Lesser Heal spellbook entry (the cast path is exercised end-to-end)
stay_start TEST test123 Ginger || exit 1
scall debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0
scall debug_set_health "$GINGER" 100000
scall debug_set_power "$GINGER" 100000
scall debug_learn_spell "$GINGER" 2050
W1=$(spawn_at "$GINGER" 51000 8)
W2=$(spawn_at "$GINGER" 51000 12)
stay_stop
echo "[orch] staged: wolves=$W1,$W2 duration=${SOAK_SECS}s"

ERRS0=$(gw_errs); PANICS0=$(mod_panics)
echo "[orch] baseline: gw_errs=$ERRS0 module_panics=$PANICS0"

# keeper: top up health/power every 20s so wolf aggro can't end the walk early (the soak itself
# must keep moving for the whole window)
(
  END=$(( $(date +%s) + SOAK_SECS ))
  while [ "$(date +%s)" -lt "$END" ]; do
    scall debug_set_health "$GINGER" 100000
    scall debug_set_power "$GINGER" 100000
    sleep 20
  done
) &
KEEPER=$!

timeout $(( SOAK_SECS + 60 )) "$WC" TEST test123 Ginger soak "$SOAK_SECS" $PAD_X $PAD_Y $PAD_Z 2050 "$W1" "$W2"
RC=$?
kill "$KEEPER" 2>/dev/null; wait "$KEEPER" 2>/dev/null
[ $RC -ne 0 ] && { echo "[orch] soak wire client exited rc=$RC"; FAILED=1; }

ERRS1=$(gw_errs); PANICS1=$(mod_panics)
echo "[orch] gateway decode/desync/error lines: $ERRS0 -> $ERRS1"
echo "[orch] module panic lines: $PANICS0 -> $PANICS1"
assert_eq "soak: zero new gateway decode/desync/error lines" "$ERRS1" "$ERRS0"
assert_eq "soak: zero new module panics" "$PANICS1" "$PANICS0"

# teardown: the pad wolves (per-guid — the seeded demo wolf stays)
for W in "$W1" "$W2"; do
  [ -n "$W" ] || continue
  sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = $W" >/dev/null
  sqlq "DELETE FROM game_creature_spawn WHERE guid = $W" >/dev/null
  sqlq "DELETE FROM game_world_entity WHERE guid = $W" >/dev/null
done
sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = $GINGER" >/dev/null

if [ "$FAILED" -eq 0 ]; then echo "[soak] PASS"; exit 0; else echo "[soak] FAIL"; exit 1; fi
