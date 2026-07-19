#!/usr/bin/env bash
# DEADMINES GROUP-COMBAT SMOKE (276 slice 3) — the bot party as a dungeon-testing vehicle:
#   1. Ginger (wire leader) + three role bots party up; Ginger fires the REAL Deadmines portal
#      (areatrigger 78 → resolve_or_create_instance → map 36, fresh instance).
#      KNOWN BUG (277): with AOI on, the instance-CREATING entry loses the SMSG_TRANSFER pair and
#      the leader limbos — so this test RELOGS her (wire session #2), and the fresh login lands
#      her INSIDE the bound instance (`pending_instance_id`). Remove the relog when 277 is fixed.
#   2. Her bots TELEPORT-FOLLOW into the same instance (goals.rs cross-map follow — server-side
#      rebuild, no client handshake needed for session-less bots).
#   3. The tank is engaged onto a real Defias pack ~45yd in; the melee chase closes, social aggro
#      drags the pack, and the role brains fight it INSIDE the instance. A keeper tops the party
#      (this proves COORDINATION in WMO corridors, not level tuning).
#   4. At least one Defias dies to the party; teardown reaps the instance.
# Straight-line movement indoors is the accepted ceiling (map 36 has no nav by design).
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight bot-deadmines

PAD_X=-8930.0; PAD_Y=-250.0; PAD_Z=80.0
DM_TRIGGER=78 # "Deadmines - Entering" → map 36 (-14.57, -385.48, 62.46)

# ---- staging: role trio + Ginger, grouped, leveled for the instance ----
scall playerbots_despawn_all || true
sqlq "DELETE FROM game_group_member WHERE character_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_group_invite WHERE target_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_instance_binding WHERE character_guid = $GINGER" >/dev/null
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z, map_id = 0 WHERE guid = $GINGER" >/dev/null
scall playerbots_spawn_role 1 $PAD_X -253.0 $PAD_Z 0 || step_fail "tank spawn failed"
scall playerbots_spawn_role 1 $PAD_X -247.0 $PAD_Z 1 || step_fail "healer spawn failed"
scall playerbots_spawn_role 1 $PAD_X -244.0 $PAD_Z 2 || step_fail "dps spawn failed"
TANK=$(char_guid Tankbot1); HEAL=$(char_guid Healbot1); DPS=$(char_guid Dpsbot1)
{ [ -z "$TANK" ] || [ -z "$HEAL" ] || [ -z "$DPS" ]; } && { echo "[orch] bot guids missing" >&2; exit 1; }
for B in "$TANK" "$HEAL" "$DPS"; do scall debug_set_level "$B" 20; done
scall debug_set_level "$GINGER" 20
echo "[orch] bot-deadmines: tank=$TANK heal=$HEAL dps=$DPS ginger=$GINGER"

HOLD=/tmp/ws_bot_dm_$$
rm -f "$HOLD" "$HOLD.ingroup"
timeout 400 "$WC" TEST test123 Ginger party-bots "$HOLD" Tankbot1 Healbot1 Dpsbot1 >/tmp/ws_bot_dm.log 2>&1 &
LEADER=$!
wait_for_file 40 "$HOLD.ingroup"
[ -f "$HOLD.ingroup" ] && step_ok "wire: 4-member party formed" || { step_fail "wire: party never formed"; tail -3 /tmp/ws_bot_dm.log; }

# ---- 1. Ginger fires the portal (instance CREATE), then relogs INTO it (the 277 workaround) ----
sleep 2
scall debug_enter_areatrigger "$GINGER" $DM_TRIGGER
INST=""
for i in $(seq 1 10); do
  INST=$(sql1 "SELECT instance_id FROM game_instance_binding WHERE character_guid = $GINGER")
  [ -n "$INST" ] && [ "$INST" != "0" ] && break
  sleep 1
done
[ -n "$INST" ] && [ "$INST" != "0" ] && step_ok "enter: portal resolved/bound instance $INST (create path)" \
  || step_fail "enter: no instance binding for Ginger"
# Drop wire session #1 (raw kill — NOT the hold release, whose disband would dissolve the party;
# the group survives a disconnect like any offline member's) and relog: the fresh login rebuilds
# Ginger INSIDE the bound instance via pending_instance_id.
kill "$LEADER" 2>/dev/null; pkill -x wire-client 2>/dev/null; sleep 2
timeout 300 "$WC" TEST test123 Ginger opcode-watch 504 240 >/tmp/ws_bot_dm2.log 2>&1 &
LEADER2=$!
GMAP=""
for i in $(seq 1 20); do
  ROW=$(sqlq "SELECT map_id, instance_id FROM game_world_entity WHERE guid = $GINGER" | sed -n 3p | tr -d ' ')
  GMAP=${ROW%%|*}
  [ "$GMAP" = "36" ] && break
  sleep 1
done
[ "$GMAP" = "36" ] && step_ok "relog: Ginger inside Deadmines instance $INST" \
  || step_fail "relog: Ginger not in map 36 (row '$ROW')"

# ---- 2. the bots teleport-follow ----
IN=0
for i in $(seq 1 20); do
  IN=$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE map_id = 36 AND instance_id = $INST AND (guid = $TANK OR guid = $HEAL OR guid = $DPS)")
  [ "${IN:-0}" = "3" ] && break
  sleep 1
done
[ "${IN:-0}" = "3" ] && step_ok "follow: all three bots teleport-followed into instance $INST" \
  || step_fail "follow: only ${IN:-0}/3 bots inside within 20s"

# ---- 3. pull a real Defias pack; the role brains fight it ----
MOB=$(sqlq "SELECT guid FROM game_world_entity WHERE map_id = 36 AND instance_id = $INST AND entry = 634" | grep -oE '[0-9]{15,}' | head -1)
[ -z "$MOB" ] && MOB=$(sqlq "SELECT guid FROM game_world_entity WHERE map_id = 36 AND instance_id = $INST AND entry = 598" | grep -oE '[0-9]{15,}' | head -1)
[ -n "$MOB" ] && step_ok "pull: found a Defias target" || step_fail "pull: no 634/598 creature in instance $INST"
scall debug_engage "$TANK" "$MOB"
FOUGHT=0; KILLED=0
for i in $(seq 1 180); do
  for M in "$TANK" "$HEAL" "$DPS" "$GINGER"; do scall debug_set_health "$M" 10000; done
  TH=$(sql1 "SELECT COUNT(*) AS n FROM game_threat WHERE source_guid = $TANK")
  [ "${TH:-0}" -ge 1 ] && FOUGHT=1
  DEAD=$(sqlq "SELECT dead FROM game_world_entity WHERE map_id = 36 AND instance_id = $INST AND (entry = 634 OR entry = 598)" | grep -c true)
  if [ "${DEAD:-0}" -ge 1 ]; then KILLED=1; break; fi
  sleep 1
done
[ "$FOUGHT" = 1 ] && step_ok "combat: the tank generated threat on the Defias pack (chase + swings landed)" \
  || step_fail "combat: tank never got threat inside the instance"
[ "$KILLED" = 1 ] && step_ok "kill: a Defias died to the bot party inside Deadmines" \
  || step_fail "kill: no Defias death within 180s"

# ---- teardown ----
kill "$LEADER2" 2>/dev/null; pkill -x wire-client 2>/dev/null
scall playerbots_despawn_all || true
sqlq "DELETE FROM game_group_member WHERE character_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_instance_binding WHERE character_guid = $GINGER" >/dev/null
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z, map_id = 0 WHERE guid = $GINGER" >/dev/null
scall debug_set_level "$GINGER" 5
[ -n "$INST" ] && scall debug_reap_instance "$INST" || true
assert_eq "teardown: zero bot rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_bot")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[bot-deadmines] PASS"; exit 0; else echo "[bot-deadmines] FAIL"; exit 1; fi
