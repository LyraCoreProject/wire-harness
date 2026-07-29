#!/usr/bin/env bash
# (class, role) rotation acceptance (work-item 153) — ONE class, THREE roles, pure data:
#   1. DEFAULTS: the rotation/kit tables self-seed; an illegal (class, role) spawn is refused.
#   2. A PALADIN party (tank/healer/dps — all class 2) + Ginger fights a staged wolf pack; per-bot
#      cast-event streams prove three DIFFERENT rotations from one class:
#      - tank: Hammer of Justice peel (853 — vanilla paladins have no taunt; the stance gate
#        killed the old class-impure 355 row, see 266) + seal upkeep (20154 aura); the
#        Consecration row (26573) is DOUBLY latent (not in the curated import + needs the 118
#        ground-AoE engine) — not asserted
#      - healer: Holy Light on damaged members (635) + blessing sweep (19740 auras)
#      - dps: seal upkeep + Judgement under TANK_ENGAGED (20271)
#   3. LIVE PATCH: a sql UPDATE swapping the dps nuke row's spell changes behavior, no republish.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight class-roles

WOLF=51002 # Test Wolf Elder (266): level 8, non-grey to the leveled bots (the L1 wolf 51000 greyed out)
PAD_X=-8890.0; PAD_Y=-460.0; PAD_Z=82.0
casts_by() { sql1 "SELECT COUNT(*) AS n FROM game_spell_cast_event WHERE caster_guid = $1 AND spell_id = $2"; }

# ---- staging ----
scall debug_seed_scenario_fixtures || true
scall playerbots_despawn_all || true
# A crashed or failed prior run leaves Ginger in a party with now-despawned bots, and the invite
# below then fails with GroupFull — which fails the party assert AND every party-scoped rotation
# assertion after it (tank peel, healer heal, blessing sweep), none of which are about grouping.
leave_any_group "$GINGER"
purge_entry_rows $WOLF
sqlq "DELETE FROM game_melee_attack" >/dev/null
sqlq "DELETE FROM game_group_member WHERE character_guid = $GINGER" >/dev/null
# Hostility on mock-seed needs staged faction rows (canonical helper; cleared in teardown).
FACTION_ROWS_BEFORE=$(sql1 "SELECT COUNT(*) AS n FROM game_faction_template")
stage_hostility
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z WHERE guid = $GINGER" >/dev/null

# ---- 1. defaults + validity gate ----
sleep 2 # >= one brain tick since publish — ensure_defaults has run
assert_ge "defaults: rotation rows self-seeded" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_rotation")" 12
assert_ge "defaults: kit rows self-seeded" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_kit")" 11
ILLEGAL=$(spacetime call "$DB" -- playerbots_spawn_class_role 1 $PAD_X $PAD_Y $PAD_Z 8 0 2>&1 | grep -c "cannot fill role")
assert_eq "validity: Mage-as-tank refused loudly" "${ILLEGAL:-0}" "1"

# ---- 2. the all-Paladin trio ----
scall playerbots_spawn_class_role 1 $PAD_X -463.0 $PAD_Z 2 0 || step_fail "paladin tank spawn failed"
scall playerbots_spawn_class_role 1 $PAD_X -457.0 $PAD_Z 2 1 || step_fail "paladin healer spawn failed"
scall playerbots_spawn_class_role 1 $PAD_X -454.0 $PAD_Z 2 2 || step_fail "paladin dps spawn failed"
TANK=$(char_guid Tankbot1); HEAL=$(char_guid Healbot1); DPS=$(char_guid Dpsbot1)
[ -z "$TANK" ] || [ -z "$HEAL" ] || [ -z "$DPS" ] && { echo "[orch] bot guids missing" >&2; exit 1; }
echo "[orch] paladins: tank=$TANK heal=$HEAL dps=$DPS"
# 266: level the bots so the cast level-gate lets their kit fire (Taunt 355 spell_level=10 is the
# ceiling; Judgement 20271=4). debug_set_level recomputes+refills vitals, so read max_health AFTER.
for B in "$TANK" "$HEAL" "$DPS"; do scall debug_set_level "$B" 10; done
for B in "$TANK" "$HEAL" "$DPS"; do
  C=$(sql1 "SELECT class FROM game_character WHERE guid = $B")
  [ "$C" = "2" ] || step_fail "bot $B is class $C, want Paladin (2)"
done
step_ok "spawn: three Paladins, three roles"
assert_eq "kit: tank learned the seal" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_spell WHERE character_guid = $TANK AND spell_id = 20154")" "1"
assert_eq "kit: healer learned Holy Light" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_spell WHERE character_guid = $HEAL AND spell_id = 635")" "1"
assert_eq "kit: dps learned Judgement" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_spell WHERE character_guid = $DPS AND spell_id = 20271")" "1"

# party up (auto-accept) so the party-scoped conditions see all four members
# Run-scoped hold path (work-item 161): defined ONCE here, passed as the party-bots hold arg.
HOLD=/tmp/ws_class_roles_$$
rm -f "$HOLD" "$HOLD.ingroup"
timeout 400 "$WC" TEST test123 Ginger party-bots "$HOLD" Tankbot1 Healbot1 Dpsbot1 >/tmp/ws_class_roles.log 2>&1 &
LEADER=$!
wait_for_file 40 "$HOLD.ingroup"
[ -f "$HOLD.ingroup" ] && step_ok "wire: 4-member all-Paladin party formed" || { step_fail "wire: party never formed"; tail -3 /tmp/ws_class_roles.log; }

# a real pack (3 wolves — enough for the Consecration ENEMIES_GE_N(3) row) on the squishies
scall debug_spawn_at_feet "$HEAL" $WOLF 2
scall debug_spawn_at_feet "$DPS" $WOLF 2
scall debug_spawn_at_feet "$DPS" $WOLF 3
WOLVES=$(sqlq "SELECT guid FROM game_world_entity WHERE entry = $WOLF" | grep -oE '[0-9]{15,}')
# 266: hold the dps at ~40% of its LEVELED max — over the dps flee_at_pct (15%, else the flee brain
# stop_attacks + legs home every tick and kills the Judgement asserts), under the healer's 80% threshold.
DPS_HOLD=$(( $(sql1 "SELECT max_health FROM game_world_entity WHERE guid = $DPS") * 40 / 100 ))
[ "${DPS_HOLD:-0}" -lt 1 ] && DPS_HOLD=25
TAUNTED=0; SEALED=0; HEALED=0; JUDGED=0
for i in $(seq 1 30); do
  for M in "$TANK" "$HEAL" "$GINGER"; do scall debug_set_health "$M" 10000; done
  scall debug_set_health "$DPS" "$DPS_HOLD"   # ~40% of leveled max — over flee, under heal threshold
  for W in $WOLVES; do scall debug_set_health "$W" 10000; done
  sleep 1
  # cast event (1s TTL) OR the stun aura (3s) — HoJ's 60s cooldown means ONE cast per window,
  # and sampling a 1s-lived event once per iteration missed real casts (266).
  { [ "$(casts_by "$TANK" 853)" -ge 1 ] || [ "$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE spell_id = 853")" -ge 1 ]; } 2>/dev/null && TAUNTED=1
  [ "$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE target_guid = $TANK AND spell_id = 20154")" -ge 1 ] 2>/dev/null && SEALED=1
  [ "$(casts_by "$HEAL" 635)" -ge 1 ] 2>/dev/null && HEALED=1
  [ "$(casts_by "$DPS" 20271)" -ge 1 ] 2>/dev/null && JUDGED=1
  if [ "$TAUNTED$SEALED$HEALED$JUDGED" = "1111" ]; then break; fi
done
[ "$TAUNTED" = 1 ] && step_ok "tank rotation: Hammer of Justice peel fired (ENEMY_ON_ALLY)" || step_fail "tank rotation: no Hammer of Justice casts"
[ "$SEALED" = 1 ] && step_ok "tank rotation: seal upkeep (SELF_MISSING_AURA aura present)" || step_fail "tank rotation: no seal aura"
# Consecration (26573, ENEMIES_ENGAGED_GE_N) not asserted: doubly latent until 118 + import (176).
[ "$HEALED" = 1 ] && step_ok "healer rotation: Holy Light on the damaged member (ALLY_HP_BELOW_PCT)" || step_fail "healer rotation: no Holy Light casts"
[ "$JUDGED" = 1 ] && step_ok "dps rotation: Judgement under TANK_ENGAGED" || step_fail "dps rotation: no Judgement casts"
# blessing sweep: every living party member ends up carrying the blessing aura
wait_for_sql_ge 20 "SELECT COUNT(*) AS n FROM game_aura WHERE spell_id = 19740" 3 \
  && step_ok "healer rotation: blessing sweep converged (>=3 members buffed)" || step_fail "healer rotation: blessing sweep never converged"

# ---- 3. live rotation patch: swap the dps nuke to Fireball, watch behavior change ----
# Scope to the PALADIN DPS row (class 2, role 2). Judgement 20271 is seeded for BOTH the paladin
# tank (COND_ALWAYS) and the paladin dps (COND_TANK_ENGAGED), and the unscoped query returned them
# in id order — so this patched the TANK's row and then waited for the DPS bot to cast the swapped
# spell, which it never had a reason to. The assertion below is about the dps; the patch must be too.
ROW=$(sqlq "SELECT id FROM pkg_playerbots_rotation WHERE spell_id = 20271 AND class = 2 AND role = 2" | grep -oE '[0-9]+' | head -1)
[ -n "$ROW" ] || { echo "[class-roles] no paladin-dps Judgement rotation row to patch" >&2; exit 1; }
sqlq "UPDATE pkg_playerbots_rotation SET spell_id = 133 WHERE id = $ROW" >/dev/null
FB0=$(casts_by "$DPS" 133); FB0=${FB0:-0}
PATCHED=0
# 270: 40 not 15. Every iteration re-stamps health and sleeps 1s, so this is a ~15s window on a
# rotation tick that has to notice the patched row, pick it, and pass the cast gates. It holds alone
# and misses under the full suite's commit stream — a deadline, not a behaviour.
for i in $(seq 1 40); do
  for W in $WOLVES; do scall debug_set_health "$W" 10000; done
  scall debug_set_health "$DPS" 10000
  sleep 1
  FB1=$(casts_by "$DPS" 133)
  [ "${FB1:-0}" -gt "$FB0" ] && PATCHED=1 && break
done
[ "$PATCHED" = 1 ] && step_ok "live patch: sql UPDATE on a rotation row changed the dps nuke (no republish)" || step_fail "live patch: dps never cast the swapped spell"
sqlq "UPDATE pkg_playerbots_rotation SET spell_id = 20271 WHERE id = $ROW" >/dev/null

# ---- teardown ----
touch "$HOLD"
wait "$LEADER"; RC_L=$?
[ $RC_L -eq 0 ] && step_ok "wire: leader flow green (disband -> DESTROYED)" || { step_fail "leader wire flow rc=$RC_L"; tail -3 /tmp/ws_class_roles.log; }
scall playerbots_despawn_all || true
purge_entry_rows $WOLF
sqlq "DELETE FROM game_melee_attack" >/dev/null
clear_hostility
rm -f "$HOLD" "$HOLD.ingroup"
assert_eq "teardown: zero bot rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_bot")" "0"
# Imported-node aware (2026-07-16): assert the faction table is RESTORED to its pre-test
# count (0 on a sandbox, 314 on an imported node) — never that it is empty.
assert_eq "teardown: faction rows restored" "$(sql1 "SELECT COUNT(*) AS n FROM game_faction_template")" "${FACTION_ROWS_BEFORE:-0}"

if [ "$FAILED" -eq 0 ]; then echo "[class-roles] PASS"; exit 0; else echo "[class-roles] FAIL"; exit 1; fi
