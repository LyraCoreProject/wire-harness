#!/usr/bin/env bash
# SCENARIO — WEAPON MASTERS (work-item 202): buy a weapon proficiency for gold from a Weapon Master
# trainer NPC. Vanilla shape: a trainer-list row whose `learn_skill_line` names a weapon line instead of
# a spell/profession — `trainer::apply_trainer_buy` forks onto the WEAPON branch (level-derived cap,
# row-presence known-check) on `skill::is_combat_skill_line`. Fixture: the seeded "Woo Ping" Weapon
# Master (51005, via debug_seed_scenario_fixtures) offers 1H Axe (skill line 44, required_level 1, 100c)
# and Polearm (skill line 229, required_level 60, 100c — the level-refusal fixture).
#
# DRIVEN VIA THE DEBUG TWIN, not a wire-client scenario mode: `debug_learn_weapon_from_trainer` (like its
# profession twin `debug_learn_profession_from_trainer`) resolves the nearest live TRAINER creature
# SERVER-SIDE from the small `character_guid` alone — the trainer's own guid is a u64 > 2^53 the
# `spacetime call` CLI mangles (JSON float precision), so unlike `test-scenario-train.sh` (which drives a
# CLASS-spell buy over the real wire protocol, where the client already holds the trainer guid from its
# own session) there is no wire-client scenario mode here; the debug reducer is the whole point of the
# exercise. A live "stay" session is still needed so the caster resolves as an in-world entity (the
# `apply_trainer_buy`/`debug_spawn_at_feet` gate).
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight scenario-weaponmaster

WEAPON_MASTER_ENTRY=51005
AXE_LINE=44      # 1H Axe: required_level 1, 100c — the buy-succeeds fixture
POLEARM_LINE=229 # Polearm: required_level 60, 100c — the level-refusal fixture
COST=100
PAD_X=-8940; PAD_Y=-150; PAD_Z=82
TEST_LEVEL=10 # deterministic: well below Polearm's required_level 60, well above 1H Axe's required_level 1

call_out() { spacetime call "$DB" -- "$@" 2>&1; } # captures the reducer's Err(String), unlike scall()

# repeatability: forget the axe line + normalize the purse + capture the level to restore in teardown
ORIG_LEVEL=$(sql1 "SELECT level FROM game_character WHERE guid = $GINGER")
sqlq "DELETE FROM game_player_skill WHERE character_guid = $GINGER AND skill_line = $AXE_LINE" >/dev/null
scall debug_set_money "$GINGER" 1000

stay_start TEST Ginger || exit 1
scall debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0
scall debug_set_level "$GINGER" $TEST_LEVEL
TRAINER=$(spawn_at "$GINGER" $WEAPON_MASTER_ENTRY 4)
if [ -z "$TRAINER" ]; then echo "[orch] weapon master spawn failed" >&2; stay_stop; exit 1; fi
echo "[orch] staged: weapon_master=$TRAINER level=$TEST_LEVEL"

# STEP 1: Polearm (required_level 60) at level 10 — refused, tagged [2] (NotEnoughSkill == level-too-low).
OUT=$(call_out debug_learn_weapon_from_trainer "$GINGER" $POLEARM_LINE)
if echo "$OUT" | grep -q '\[2\]'; then step_ok "polearm buy refused below required level ([2] tag)"; else step_fail "polearm buy: expected a [2] refusal, got: $OUT"; fi
assert_eq "polearm refusal: no skill row created" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_skill WHERE character_guid = $GINGER AND skill_line = $POLEARM_LINE")" "0"
# mid-session, the authoritative purse is the LIVE entity (game_character.money persists at logout)
assert_eq "polearm refusal: money unchanged (no charge)" "$(sql1 "SELECT money FROM game_world_entity WHERE guid = $GINGER")" "1000"

# STEP 2: 1H Axe (required_level 1) at level 10 — succeeds: skill row born at 1/(level*5), money -100.
OUT=$(call_out debug_learn_weapon_from_trainer "$GINGER" $AXE_LINE)
if echo "$OUT" | grep -qi 'error\|\[[0-9]\]'; then step_fail "axe buy: expected success, got: $OUT"; else step_ok "axe buy succeeded"; fi
ROW=$(sqlq "SELECT current, max_rank FROM game_player_skill WHERE character_guid = $GINGER AND skill_line = $AXE_LINE" | sed -n 3p)
CUR=$(awk -F'|' '{gsub(/ /,"",$1); print $1}' <<<"$ROW"); MAXR=$(awk -F'|' '{gsub(/ /,"",$2); print $2}' <<<"$ROW")
assert_eq "axe buy: skill born at current=1" "${CUR:-0}" "1"
assert_eq "axe buy: max_rank == level*5 ($TEST_LEVEL*5)" "${MAXR:-0}" "$(( TEST_LEVEL * 5 ))"
assert_eq "axe buy: money 1000 -> 900 (cost $COST)" "$(sql1 "SELECT money FROM game_world_entity WHERE guid = $GINGER")" "900"

# STEP 3: re-buy 1H Axe — refused as already-known (row PRESENCE, not a cap comparison), no re-charge.
OUT=$(call_out debug_learn_weapon_from_trainer "$GINGER" $AXE_LINE)
if echo "$OUT" | grep -q '\[0\]'; then step_ok "axe re-buy refused as already-known ([0] tag)"; else step_fail "axe re-buy: expected a [0] refusal, got: $OUT"; fi
assert_eq "axe re-buy: money unchanged (no re-charge)" "$(sql1 "SELECT money FROM game_world_entity WHERE guid = $GINGER")" "900"
assert_eq "axe re-buy: still exactly one skill row" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_skill WHERE character_guid = $GINGER AND skill_line = $AXE_LINE")" "1"

# ---- teardown (asserted): restore level/money, forget the skill, remove the pad spawn ----
scall debug_set_level "$GINGER" "${ORIG_LEVEL:-1}"
stay_stop
scall debug_set_money "$GINGER" 0
sqlq "DELETE FROM game_player_skill WHERE character_guid = $GINGER AND skill_line = $AXE_LINE" >/dev/null
sqlq "DELETE FROM game_creature_spawn WHERE entry = $WEAPON_MASTER_ENTRY AND x > -8950" >/dev/null
sqlq "DELETE FROM game_world_entity WHERE guid = ${TRAINER:-0}" >/dev/null
assert_eq "teardown: pad weapon master gone" "$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE guid = ${TRAINER:-0}")" "0"
assert_eq "teardown: axe skill row removed" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_skill WHERE character_guid = $GINGER AND skill_line = $AXE_LINE")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[scenario-weaponmaster] PASS"; exit 0; else echo "[scenario-weaponmaster] FAIL"; exit 1; fi
