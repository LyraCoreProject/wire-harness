#!/usr/bin/env bash
# EventAI TIMED_IN_COMBAT CAST (193-CAST) — fully headless, ZERO wire sessions:
#   A live-inserted CAST rule on the Test Wolf Elder (Frostbolt 116, 1-2s initial, 4-6s repeat)
#   makes an engaged elder CAST at its melee victim on the timer: the pass arms the timer off the
#   melee row alone (works for retaliation pulls — no aggro hook needed), fires via the same
#   begin_cast the creature rotation uses, and re-arms. Asserts the Frostbolt AURA landing on the
#   victim (durable, unlike 1s-TTL cast events) and the timer row lifecycle (armed, then reaped
#   after the fight ends).
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight eventai-cast

WOLF=51002
PAD_X=-8930.0; PAD_Y=-250.0; PAD_Z=80.0

# ---- staging ----
scall playerbots_despawn_all || true
purge_entry_rows $WOLF
sqlq "DELETE FROM game_melee_attack" >/dev/null
scall debug_seed_creature_ai_fixtures || true   # re-seed (clears) FIRST, then add the test rule
sqlq "INSERT INTO game_creature_ai_event (id, creature_entry, event_type, action_type, text, spell_id, initial_min_ms, initial_max_ms, repeat_min_ms, repeat_max_ms) VALUES (0, $WOLF, 1, 2, '', 116, 1000, 2000, 4000, 6000)" >/dev/null
RULES=$(sql1 "SELECT COUNT(*) AS n FROM game_creature_ai_event WHERE creature_entry = $WOLF")
assert_ge "staging: the elder CAST rule landed" "${RULES:-0}" 1

scall playerbots_spawn_role 1 $PAD_X $PAD_Y $PAD_Z 0 || { echo "[orch] bot spawn failed" >&2; exit 1; }
BOT=$(char_guid Tankbot1)
[ -z "$BOT" ] && { echo "[orch] no bot guid" >&2; exit 1; }
scall debug_set_level "$BOT" 10
scall debug_spawn_at_feet "$BOT" $WOLF 3
W=$(sqlq "SELECT guid FROM game_world_entity WHERE entry = $WOLF" | grep -oE '[0-9]{15,}' | head -1)
echo "[orch] eventai-cast: bot=$BOT wolf=$W"

# ---- 1. fight starts (elder aggro or bot grind — either way a melee row appears) ----
ENGAGED=0
for i in $(seq 1 30); do
  M=$(sql1 "SELECT COUNT(*) AS n FROM game_melee_attack WHERE attacker_guid = $W")
  [ "${M:-0}" -ge 1 ] && ENGAGED=1 && break
  sleep 1
done
[ "$ENGAGED" = 1 ] && step_ok "fight: the elder is swinging (melee row up)" || step_fail "fight: elder never engaged within 30s"

# ---- 2. the timer arms and the cast fires: Frostbolt aura on the victim ----
wait_for_sql_ge 15 "SELECT COUNT(*) AS n FROM game_creature_ai_timer WHERE creature_guid = $W" 1 \
  && step_ok "timer: armed off the melee row (no hook needed)" || step_fail "timer: never armed within 15s"
CAST=0
for i in $(seq 1 30); do
  scall debug_set_health "$BOT" 10000   # keeper: the bot must outlive the observation
  scall debug_set_health "$W" 10000     # and the wolf must outlive the bot's grind
  A=$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE target_guid = $BOT AND spell_id = 116")
  [ "${A:-0}" -ge 1 ] && CAST=1 && break
  sleep 1
done
[ "$CAST" = 1 ] && step_ok "cast: Frostbolt 116 landed on the victim (timed EventAI cast fired)" \
  || step_fail "cast: no Frostbolt aura on the victim within 30s"

# ---- 3. lifecycle: fight ends -> the timer reaps ----
purge_entry_rows $WOLF
sqlq "DELETE FROM game_melee_attack" >/dev/null
wait_for_sql_ge 10 "SELECT COUNT(*) AS n FROM game_creature_ai_timer WHERE creature_guid = $W" 0 2>/dev/null
LEFT=$(sql1 "SELECT COUNT(*) AS n FROM game_creature_ai_timer WHERE creature_guid = $W")
sleep 3
LEFT=$(sql1 "SELECT COUNT(*) AS n FROM game_creature_ai_timer WHERE creature_guid = $W")
assert_eq "lifecycle: the timer reaped once the fight ended" "${LEFT:-1}" "0"

# ---- teardown ----
scall playerbots_despawn_all || true
scall debug_seed_creature_ai_fixtures || true   # drop the test rule, restore the canonical set
sqlq "DELETE FROM game_melee_attack" >/dev/null
assert_eq "teardown: zero bot rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_bot")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[eventai-cast] PASS"; exit 0; else echo "[eventai-cast] FAIL"; exit 1; fi
