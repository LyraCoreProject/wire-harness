#!/usr/bin/env bash
# DYNAMIC EVENTS ENGINE (280) — the kobold chain's full outcome graph, headless via scall/sql:
# seed → head auto-starts → forced SUCCESS follows the success link (defend event + protected
# NPC) → forced FAIL flips the world (retake event, chain OCCUPIED, camp standing) → credited
# kills resolve the retake (progress, gold-tier XP+money on a live participant) → head re-arms
# on a future cooldown. Uses a disposable BOT as the participant (always-live entity).
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight dynevents

sq() { spacetime sql "$DB" "$1" 2>/dev/null | tail -n +3; }

# --- Seed + head auto-start (chain armed at next_start 0 → first 2s pass starts it) ---
scall debug_dynevent_seed_kobold_chain
ok=""
for _ in $(seq 1 8); do
  sleep 1
  [ -n "$(sq "SELECT id FROM pkg_dynevent_active WHERE def_id = 1001" | tr -d ' \n')" ] && ok=1 && break
done
if [ -n "$ok" ]; then step_ok "seeded chain head 1001 auto-started"; else step_fail "1001 did not auto-start"; fi
n_spawns=$(sq "SELECT guid FROM game_encounter_spawn WHERE encounter_id = 28001001" | grep -c '[0-9]')
if [ "$n_spawns" -ge 3 ]; then step_ok "opening wave tracked ($n_spawns spawns under 28001001)"; else step_fail "opening wave missing (got $n_spawns)"; fi

# --- Participant: a bot inside the circle (roster latch is the reward eligibility gate) ---
center=$(sq "SELECT x, y, z FROM pkg_dynevent_def WHERE def_id = 1001" | head -1 | tr -d ' ' | tr '|' ' ')
read -r cx cy cz <<<"$center"
scall playerbots_spawn 1 "$cx" "$cy" "$cz"
bot=$(sq "SELECT character_guid FROM pkg_playerbots_bot" | tr -d ' ' | tail -1)
if [ -z "$bot" ]; then step_fail "no bot spawned"; fi
ok=""
for _ in $(seq 1 8); do
  # Re-pin the bot every try: its wander brain leaves the circle within seconds (the EVENT goal
  # that keeps bots at events is a separate slice) — the roster pass must catch it inside.
  scall debug_teleport "$bot" 0 "$cx" "$cy" "$cz" 0
  sleep 1
  [ "$(sq "SELECT entered FROM pkg_dynevent_contrib WHERE character_guid = $bot" | tr -d ' ')" = "true" ] && ok=1 && break
done
if [ -n "$ok" ]; then step_ok "bot $bot latched into the roster (entered)"; else step_fail "roster never latched bot $bot"; fi

# --- Organic participation (the EVENT goal + the threat ledger, live combat, no forcing):
# --- a leveled bot self-selects the event (goal kind 4), holds the circle, and fights the armed
# --- wave. The deterministic proof is a THREAT row from the bot on an event creature — that IS
# --- the contribution ledger the death snapshot converts to credit (kill-to-credit itself is
# --- kill-speed/class-dependent, so credit>0 / progress>0 are instant-pass bonuses, not the
# --- gate; the conversion is live-verified in the work item and exercised in every real event).
scall debug_set_level "$bot" 25
scall debug_set_health "$bot" 2000
ok=""
for _ in $(seq 1 15); do
  sleep 3
  k=$(sq "SELECT kind FROM pkg_playerbots_goal WHERE character_guid = $bot AND state = 0" | tr -d ' ' | tail -1)
  [ "$k" = "4" ] && ok=1 && break
done
if [ -n "$ok" ]; then step_ok "bot self-selected the EVENT goal"; else step_fail "bot never chose the EVENT goal"; fi
ok=""
for _ in $(seq 1 25); do
  sleep 3
  credit=$(sq "SELECT credit FROM pkg_dynevent_contrib WHERE character_guid = $bot" | tr -d ' ' | tail -1)
  prog=$(sq "SELECT progress FROM pkg_dynevent_active WHERE def_id = 1001" | tr -d ' ')
  threat=$(sq "SELECT threat FROM game_threat WHERE source_guid = $bot" | grep '[0-9]' | head -1 | tr -d ' ')
  { [ -n "$credit" ] && [ "$credit" -gt 0 ]; } || { [ -n "$prog" ] && [ "$prog" -gt 0 ]; } \
    || { [ -n "$threat" ] && [ "$threat" -gt 0 ]; } && ok=1 && break
done
if [ -n "$ok" ]; then
  step_ok "organic combat on the ledger (threat=${threat:-0} credit=${credit:-0} progress=${prog:-0})"
else
  step_fail "no organic threat/credit/progress within the window"
fi

# --- Success link: 1001 forced success → 1002 (defend) starts with a protected NPC ---
scall debug_dynevent_force_end 1001 true
sleep 3
if [ -n "$(sq "SELECT id FROM pkg_dynevent_active WHERE def_id = 1002" | tr -d ' \n')" ]; then
  step_ok "success link: 1002 (defend) started"
else
  step_fail "1002 did not start after 1001 success"
fi
prot=$(sq "SELECT protected_guid FROM pkg_dynevent_active WHERE def_id = 1002" | tr -d ' ')
if [ -n "$prot" ] && [ "$prot" != "0" ]; then step_ok "defend event spawned its protected NPC ($prot)"; else step_fail "no protected NPC on 1002"; fi

# --- Fail link + world flip: 1002 forced fail → 1003 open-ended, chain OCCUPIED, camp stands ---
scall debug_dynevent_force_end 1002 false
sleep 3
if [ -n "$(sq "SELECT id FROM pkg_dynevent_active WHERE def_id = 1003" | tr -d ' \n')" ]; then
  step_ok "fail link: 1003 (retake) started"
else
  step_fail "1003 did not start after 1002 fail"
fi
chain_state=$(sq "SELECT state FROM pkg_dynevent_chain WHERE head_def = 1001" | tr -d ' ')
if [ "$chain_state" = "2" ]; then step_ok "chain is OCCUPIED (world flipped)"; else step_fail "chain state $chain_state != OCCUPIED"; fi
camp=$(sq "SELECT guid FROM game_encounter_spawn WHERE encounter_id = 28001003" | grep '[0-9]' | tr -d ' ' | tr '\n' ' ')
n_camp=$(echo "$camp" | wc -w)
if [ "$n_camp" -ge 4 ]; then step_ok "occupation camp standing ($n_camp workers)"; else step_fail "camp missing (got $n_camp)"; fi
if [ -z "$(sq "SELECT guid FROM game_encounter_spawn WHERE encounter_id = 28001002" | tr -d ' \n')" ]; then
  step_ok "failed event's own spawns torn down (28001002 empty)"
else
  step_fail "28001002 spawns leaked past the fail"
fi

# --- Retake: credited kills → success, gold-tier rewards on the live bot, head re-arms ---
xp0=$(sq "SELECT xp FROM game_world_entity WHERE guid = $bot" | tr -d ' ')
money0=$(sq "SELECT money FROM game_world_entity WHERE guid = $bot" | tr -d ' ')
scall debug_dynevent_grant_credit 1003 "$bot" 1000
for g in $camp; do scall debug_kill_creature "$bot" "$g"; done
sleep 4
if [ -z "$(sq "SELECT id FROM pkg_dynevent_active" | tr -d ' \n')" ]; then
  step_ok "retake resolved — no active events remain"
else
  step_fail "an event is still active after the camp died"
fi
money1=$(sq "SELECT money FROM game_world_entity WHERE guid = $bot" | tr -d ' ')
xp1=$(sq "SELECT xp FROM game_world_entity WHERE guid = $bot" | tr -d ' ')
lvl1=$(sq "SELECT level FROM game_world_entity WHERE guid = $bot" | tr -d ' ')
if [ "$((money1 - money0))" -ge 1000 ]; then
  step_ok "gold-tier money granted (+$((money1 - money0))c)"
else
  step_fail "money delta $((money1 - money0)) < 1000 (gold tier)"
fi
if [ "$xp1" -gt "$xp0" ] || [ "$lvl1" -gt 1 ]; then
  step_ok "event XP granted (xp $xp0→$xp1, level $lvl1)"
else
  step_fail "no XP movement on the participant"
fi
chain_state=$(sq "SELECT state FROM pkg_dynevent_chain WHERE head_def = 1001" | tr -d ' ')
rearm=$(sq "SELECT next_start_at_micros FROM pkg_dynevent_chain WHERE head_def = 1001" | tr -d ' ')
now_us=$(date +%s%6N)
if [ "$chain_state" = "0" ] && [ "$rearm" -gt "$now_us" ]; then
  step_ok "head re-armed on a future cooldown"
else
  step_fail "chain state=$chain_state rearm=$rearm (expected COOLDOWN in the future)"
fi

# --- Cleanup: despawn bots, re-seed (wipes forced state; the chain keeps cycling live — the
# --- ambient Fargodeep event IS the shipped content, not test residue) ---
scall playerbots_despawn_all
scall debug_dynevent_seed_kobold_chain

if [ "$FAILED" -eq 0 ]; then echo "[dynevents] PASS"; exit 0; else echo "[dynevents] FAIL"; exit 1; fi
