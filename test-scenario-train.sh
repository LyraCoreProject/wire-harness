#!/usr/bin/env bash
# SCENARIO 3 — TRAIN-AND-CAST (work-item 140): buy a trainer spell over the wire (SMSG_TRAINER_LIST
# -> SMSG_TRAINER_BUY_SUCCEEDED + SMSG_LEARNED_SPELL) -> cast it (SMSG_SPELL_START(1500) ->
# SMSG_SPELL_GO) -> assert the effect landed (health rose by the heal) + money/spellbook state.
# Fixture: the seeded Profession Trainer (51001) offers Lesser Heal (2050, 100c) via
# debug_seed_scenario_fixtures.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight scenario-train

TRAINER_ENTRY=51001; SPELL=2050; CAST_MS=1500; COST=100
PAD_X=-8880; PAD_Y=-240; PAD_Z=82
# Run-scoped handshake path (work-item 161): defined ONCE here, passed as a wire-client arg.
TRAIN_READY=/tmp/ws_train_ready_$$

# repeatability: forget the spell + normalize the purse before buying it again
sqlq "DELETE FROM game_player_spell WHERE character_guid = $GINGER AND spell_id = $SPELL" >/dev/null
settle_char_money "$GINGER" # the 265 trace caught this exact clobber (stale 800 over staged 1000)
scall debug_set_money "$GINGER" 1000

stay_start TEST Ginger || exit 1
scall debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0
TRAINER=$(spawn_at "$GINGER" $TRAINER_ENTRY 4)
stay_stop
if [ -z "$TRAINER" ]; then echo "[orch] trainer spawn failed" >&2; exit 1; fi
echo "[orch] staged: trainer=$TRAINER"

# ---- run the wire scenario; stage the damaged caster at its ready-handshake ----
rm -f "$TRAIN_READY"
timeout 120 "$WC" TEST Ginger scenario-train "$TRAINER" $SPELL $CAST_MS "$TRAIN_READY" &
WIRE=$!
HEALTH0=""
wait_for_file 30 "$TRAIN_READY"
if [ -f "$TRAIN_READY" ]; then
  MAXHP=$(sql1 "SELECT max_health FROM game_world_entity WHERE guid = $GINGER")
  # A 60-hole was regen-closable under suite load (in-suite the cast window stretches and
  # out-of-combat regen shrank the deficit below the heal size -> overheal-capped +31, assert
  # wants >= 50). 150 keeps a >heal hole through any realistic window; clamped to stay >= 1 HP.
  HOLE=150; [ "${MAXHP:-100}" -le "$HOLE" ] && HOLE=$(( ${MAXHP:-100} - 1 ))
  HEALTH0=$(( ${MAXHP:-100} - HOLE ))
  scall debug_set_health "$GINGER" "$HEALTH0"
  scall debug_set_power "$GINGER" 100 # no mana curve on a no-import sandbox -> stage the 30-mana cost
  rm -f "$TRAIN_READY" # signals the wire client to proceed
else
  echo "[orch] wire client never signalled ready" >&2; FAILED=1
fi
wait "$WIRE"; RC=$?
[ $RC -ne 0 ] && { echo "[orch] wire scenario failed (rc=$RC)"; FAILED=1; }

# ---- server-state assertions ----
# 265 ROOT CAUSE (2026-07-16, instrumented persist trace): the buy is entity-charged and the 900
# reaches game_character only via the gateway's ASYNC logout teardown persist, which lands ~1-3s
# AFTER `wait $WIRE` returns — an immediate read races it and sees the pre-buy 1000. (The old
# "settles at 0" observation was this script's OWN teardown `debug_set_money 0`, misread as
# corruption.) Server-side persistence is correct — settle-poll instead of a single read.
MONEY_SETTLED=""
for _ in $(seq 1 12); do
  MONEY_SETTLED=$(sql1 "SELECT money FROM game_character WHERE guid = $GINGER")
  [ "$MONEY_SETTLED" = "900" ] && break
  sleep 1
done
assert_eq "buy: money 1000 -> 900 (cost $COST, persist-settled)" "$MONEY_SETTLED" "900"
assert_ge "learn: spellbook row for $SPELL exists" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_spell WHERE character_guid = $GINGER AND spell_id = $SPELL")" 1
# The heal is +50; out-of-combat regen can add a little more inside the window, so assert >= +50.
HEALTH1=$(sql1 "SELECT health FROM game_world_entity WHERE guid = $GINGER")
[ -z "$HEALTH1" ] && HEALTH1=$(sql1 "SELECT health FROM game_character WHERE guid = $GINGER") # logged out already
# Lesser Heal 2050 is base 46 + d11 (rolls 47..57 at the floor) — the old ">= 50" assert failed a
# legal 47-49 roll roughly 1 run in 4. Assert the ROLL FLOOR; regen only inflates the delta.
assert_ge "cast effect: health rose by >= the heal roll floor (46+d11)" "$(( ${HEALTH1:-0} - ${HEALTH0:-0} ))" 46

# ---- teardown (asserted): the pad trainer only (the seeded start-area trainer stays) ----
sqlq "DELETE FROM game_creature_spawn WHERE entry = $TRAINER_ENTRY AND x > -8890" >/dev/null
sqlq "DELETE FROM game_world_entity WHERE guid = ${TRAINER:-0}" >/dev/null
scall debug_set_money "$GINGER" 0
assert_eq "teardown: pad trainer gone" "$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE guid = ${TRAINER:-0}")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[scenario-train] PASS"; exit 0; else echo "[scenario-train] FAIL"; exit 1; fi
