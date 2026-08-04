#!/usr/bin/env bash
# DEADMINES GROUP-COMBAT SMOKE (276 slice 3) — the bot party as a dungeon-testing vehicle:
#   1. Ginger (wire leader) + three role bots party up; Ginger fires the REAL Deadmines portal
#      (areatrigger 78 → resolve_or_create_instance → map 36, fresh instance). The TRANSFER pair
#      rides the coordinator connection (277 fix) and the held wire session auto-acks
#      SMSG_NEW_WORLD — she lands inside directly, no relog.
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

# Return Ginger to the world database from whichever shard currently holds her, using the sequence
# test-transfer-crash-matrix.sh::bring_home proved: materialise her live on the holder, cross-map
# teleport (writes the destination + despawns the entity), then a login drives `settle_transfer`.
#
# Needed at BOTH ends. At teardown, because the character row lives on the instance shard once the
# `36:*` rule routes the entry there — so the position/map UPDATE below would write to a database
# that no longer holds her and she would stay parked in the dungeon shard, where the NEXT test
# cannot find her at all ("fixture characters missing"). At staging, because a previous run that
# crashed mid-instance left her exactly there.
ginger_home() {
  local holder
  for holder in lyracore-instances; do
    [ "$holder" = "$DB" ] && continue
    [ "$(sql1 "SELECT COUNT(*) AS n FROM game_character WHERE guid = $GINGER" "$holder")" = "1" ] || continue
    echo "[orch] Ginger is on '$holder' — bringing her home to '$DB'"
    if stay_start TEST test123 Ginger 60 "$holder"; then
      spacetime call "$holder" -- debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0 >/dev/null 2>&1
      stay_stop
      timeout 90 "$WC" TEST test123 Ginger logout >/dev/null 2>&1
    fi
  done
}

# ---- staging: role trio + Ginger, grouped, leveled for the instance ----
ginger_home
leave_any_group "$GINGER"   # a crashed run leaves her in a party; the next invite then says GroupFull
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

# ---- 1. Ginger fires the portal (instance CREATE) and lands INSIDE — no relog (277 fixed) ----
sleep 2
scall debug_enter_areatrigger "$GINGER" $DM_TRIGGER
# WHICH DATABASE the instance lives on is a deployment fact, not a constant: with a `36:*` shard-map
# rule the entry TRANSFERS the character to the instances shard, and every query below then finds
# nothing on the default database — reported as "Ginger not live in a map-36 instance" while the
# gateway log shows the entry succeeding. Probe both and remember the winner in $IDB. A single-
# database realm resolves to $DB on the first candidate and behaves exactly as before.
INST=""; GMAP=""; IDB=$DB
for i in $(seq 1 15); do
  for CAND in "$DB" lyracore-instances; do
    ROW=$(sqlq "SELECT map_id, instance_id FROM game_world_entity WHERE guid = $GINGER" "$CAND" | sed -n 3p | tr -d ' ')
    GMAP=${ROW%%|*}; INST=${ROW##*|}
    if [ "$GMAP" = "36" ] && [ -n "$INST" ] && [ "$INST" != "0" ]; then IDB=$CAND; break 2; fi
  done
  sleep 1
done
[ "$IDB" = "$DB" ] || echo "[orch] the instance lives on '$IDB' (shard-map routed) — querying it for every instance-side assert"
# …and STOP there when it is a different database, because the thing this test measures next cannot
# happen in that topology. `goals.rs`'s teleport-follow reads the LEADER'S ENTITY out of its own
# database and teleports the bot to `l.map_id / l.instance_id` — with map 36 routed to another
# shard the leader has no row where the bots are, so the follow cannot even see that she left. A
# module cannot read another database; this needs the gateway to drive the bots' transfer the way
# it drives a player's. Reported as a SKIP with the reason rather than a FAIL, because nothing here
# is broken — the capability is unbuilt. Run this test on a single-database realm to exercise it.
if [ "$IDB" != "$DB" ]; then
  echo "SKIP: map 36 routes to '$IDB' and cross-DATABASE bot teleport-follow is not implemented" \
       "(goals.rs reads the leader's entity from its own database) — run on a single-database realm"
  leave_any_group "$GINGER"
  ginger_home
  sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z, map_id = 0 WHERE guid = $GINGER" >/dev/null
  scall playerbots_despawn_all || true
  scall_on "$IDB" playerbots_despawn_all || true
  [ -n "$INST" ] && scall_on "$IDB" debug_reap_instance "$INST"
  kill "$LEADER" 2>/dev/null; pkill -x wire-client 2>/dev/null
  exit 77
fi
[ "$GMAP" = "36" ] && [ "$INST" != "0" ] \
  && step_ok "enter: portal created instance $INST and Ginger landed INSIDE (coordinator-relayed transfer + auto-ack, no relog)" \
  || step_fail "enter: Ginger not live in a map-36 instance (row '$ROW')"

# ---- 2. the bots teleport-follow ----
IN=0
for i in $(seq 1 20); do
  IN=$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE map_id = 36 AND instance_id = $INST AND (guid = $TANK OR guid = $HEAL OR guid = $DPS)" "$IDB")
  [ "${IN:-0}" = "3" ] && break
  sleep 1
done
[ "${IN:-0}" = "3" ] && step_ok "follow: all three bots teleport-followed into instance $INST" \
  || step_fail "follow: only ${IN:-0}/3 bots inside within 20s"

# ---- 3. pull a real Defias pack; the role brains fight it ----
MOB=$(sqlq "SELECT guid FROM game_world_entity WHERE map_id = 36 AND instance_id = $INST AND entry = 634" "$IDB" | grep -oE '[0-9]{15,}' | head -1)
[ -z "$MOB" ] && MOB=$(sqlq "SELECT guid FROM game_world_entity WHERE map_id = 36 AND instance_id = $INST AND entry = 598" "$IDB" | grep -oE '[0-9]{15,}' | head -1)
[ -n "$MOB" ] && step_ok "pull: found a Defias target" || step_fail "pull: no 634/598 creature in instance $INST"
# Both melee bots engage (the orchestrator IS the player's pull here — a real run walks the party
# in): the melee chase carries them the ~45yd to the pack together. The dps alone would hold
# formation at the entrance, outside its 20yd assist radius (the known healer/positioning gap in
# 276 — the party anchors on the leader, not the fight).
# …on the instance shard: tank/heal/dps followed Ginger through the portal, so they are no longer
# on $DB and a call there would no-op silently.
scall_on "$IDB" debug_engage "$TANK" "$MOB"
scall_on "$IDB" debug_engage "$DPS" "$MOB"
FOUGHT=0; KILLED=0
for i in $(seq 1 300); do
  for M in "$TANK" "$HEAL" "$DPS" "$GINGER"; do scall_on "$IDB" debug_set_health "$M" 10000; done
  TH=$(sql1 "SELECT COUNT(*) AS n FROM game_threat WHERE source_guid = $TANK" "$IDB")
  [ "${TH:-0}" -ge 1 ] && FOUGHT=1
  DEAD=$(sqlq "SELECT dead FROM game_world_entity WHERE map_id = 36 AND instance_id = $INST AND (entry = 634 OR entry = 598)" "$IDB" | grep -c true)
  if [ "${DEAD:-0}" -ge 1 ]; then KILLED=1; break; fi
  sleep 1
done
[ "$FOUGHT" = 1 ] && step_ok "combat: the tank generated threat on the Defias pack (chase + swings landed)" \
  || step_fail "combat: tank never got threat inside the instance"
[ "$KILLED" = 1 ] && step_ok "kill: a Defias died to the bot party inside Deadmines" \
  || step_fail "kill: no Defias death within 300s"

# ---- teardown ----
kill "$LEADER" 2>/dev/null; pkill -x wire-client 2>/dev/null
scall playerbots_despawn_all || true
[ "${IDB:-$DB}" = "$DB" ] || scall_on "$IDB" playerbots_despawn_all || true
leave_any_group "$GINGER"
ginger_home
assert_eq "teardown: Ginger came home to '$DB'" "$(sql1 "SELECT COUNT(*) AS n FROM game_character WHERE guid = $GINGER")" "1"
sqlq "DELETE FROM game_group_member WHERE character_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_instance_binding WHERE character_guid = $GINGER" >/dev/null
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z, map_id = 0 WHERE guid = $GINGER" >/dev/null
scall debug_set_level "$GINGER" 5
[ -n "$INST" ] && scall debug_reap_instance "$INST" || true
assert_eq "teardown: zero bot rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_bot")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[bot-deadmines] PASS"; exit 0; else echo "[bot-deadmines] FAIL"; exit 1; fi
