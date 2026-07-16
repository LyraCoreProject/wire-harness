#!/usr/bin/env bash
# SCENARIO 4 — DEATH LOOP (work-item 140): die to a mob (real melee killing blow) -> release
# (SMSG_CORPSE_RECLAIM_DELAY 30s) -> wait out the delay -> reclaim -> assert 50%-health resurrect,
# NO rez sickness from a corpse reclaim, death durability loss, and clean combat state. Then the
# spirit-healer sickness LEVEL GATE (vanilla: sickness only at level >= 11) via the shared
# do_spirit_healer_res path, asserted both sides of the boundary.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight scenario-death

WOLF_ENTRY=51000; SICKNESS=15007
PAD_X=-8860; PAD_Y=-270; PAD_Z=82
# Run-scoped handshake paths (work-item 161): defined ONCE here, passed as wire-client args.
DEATH_READY=/tmp/ws_death_ready_$$
DEATH_RECLAIMED=/tmp/ws_death_reclaimed_$$
# u64 corpse guid exceeds bash's signed 64-bit arithmetic — compute in python
CORPSE=$(python3 -c "print((0xF101 << 48) | $GINGER)")

# stage: level 10 (below the sickness gate), full health, a wolf waiting on the pad
scall debug_set_level "$GINGER" 10
sqlq "DELETE FROM game_aura WHERE target_guid = $GINGER AND spell_id = $SICKNESS" >/dev/null
# Reset the corpse-reclaim ESCALATION ladder (226): other suite tests kill Ginger too, and a repeat
# death inside the escalation window waits 60s where this test asserts the first-death 30s.
sqlq "UPDATE game_character SET death_expire_micros = 0 WHERE guid = $GINGER" >/dev/null
stay_start TEST test123 Ginger || exit 1
scall debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0
scall debug_set_health "$GINGER" 100000
WOLF=$(spawn_at "$GINGER" $WOLF_ENTRY 5)
# the durability claim needs an equipped weapon; grant a starter sword if the slot is empty
# (equipped over the wire below — debug_equip_weapon upserts by slot-derived guid and can collide
# with a previously-unequipped instance)
NEED_EQUIP=""
if [ -z "$(sql1 "SELECT durability FROM game_item_instance WHERE owner_guid = $GINGER AND slot = 15")" ]; then
  scall debug_grant_item "$GINGER" 25 1
  NEED_EQUIP=1
fi
stay_stop
if [ -n "$NEED_EQUIP" ]; then
  SWORD_SLOT=$(sqlq "SELECT slot FROM game_item_instance WHERE owner_guid = $GINGER AND entry = 25" | grep -oE '[0-9]+' | tail -1)
  [ -n "$SWORD_SLOT" ] && timeout 60 "$WC" TEST test123 Ginger equip-from "$SWORD_SLOT" >/dev/null 2>&1
fi
if [ -z "$WOLF" ]; then echo "[orch] wolf spawn failed" >&2; exit 1; fi
# repeatability: every suite death costs 10% max durability and nothing ever repaired the starter
# sword — at the 0 floor the loss assert below can't observe a delta. Top it up by guid (compound
# UPDATE no-ops — resolve the guid first).
MH_GUID=$(sql1 "SELECT guid FROM game_item_instance WHERE owner_guid = $GINGER AND slot = 15")
[ -n "$MH_GUID" ] && sqlq "UPDATE game_item_instance SET durability = 20 WHERE guid = $MH_GUID" >/dev/null
DUR0=$(sql1 "SELECT durability FROM game_item_instance WHERE owner_guid = $GINGER AND slot = 15")
echo "[orch] staged: wolf=$WOLF corpse_guid=$CORPSE mainhand_dur0=${DUR0:-none}"

# ---- run the wire scenario; arrange the real death at its ready-handshake ----
rm -f "$DEATH_READY" "$DEATH_RECLAIMED"
timeout 180 "$WC" TEST test123 Ginger scenario-death "$CORPSE" "$DEATH_READY" "$DEATH_RECLAIMED" &
WIRE=$!
wait_for_file 30 "$DEATH_READY"
if [ -f "$DEATH_READY" ]; then
  scall debug_set_health "$GINGER" 1
  scall debug_engage "$WOLF" "$GINGER" # the wolf lands the real killing blow
  DEAD=""
  for _ in $(seq 1 30); do
    DEAD=$(sqlq "SELECT dead FROM game_world_entity WHERE guid = $GINGER" | grep -c true)
    [ "$DEAD" = "1" ] && break
    sleep 1
  done
  assert_eq "death: entity died to the mob's swing" "$DEAD" "1"
  rm -f "$DEATH_READY" # signal: death confirmed, release now
else
  echo "[orch] wire client never signalled ready" >&2; FAILED=1
fi

# after release: the corpse row must exist while the wire client waits out the 30s delay
CORPSE_SEEN=0
for _ in $(seq 1 15); do
  CORPSE_SEEN=$(sql1 "SELECT COUNT(*) AS n FROM game_corpse WHERE guid = $CORPSE")
  [ "${CORPSE_SEEN:-0}" -ge 1 ] && break
  sleep 1
done
assert_ge "release: corpse row exists" "${CORPSE_SEEN:-0}" 1
# the GHOST-RUN leg: release teleports the ghost to the graveyard (the start area), beyond the 39yd
# reclaim radius — run the ghost back to its corpse (debug_teleport stands in for the walk)
scall debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0

# wait for the wire client's reclaim, then assert the resurrected state and let it finish
wait_for_file 60 "$DEATH_RECLAIMED"
if [ -f "$DEATH_RECLAIMED" ]; then
  sleep 2
  HP=$(sql1 "SELECT health FROM game_world_entity WHERE guid = $GINGER")
  MAXHP=$(sql1 "SELECT max_health FROM game_world_entity WHERE guid = $GINGER")
  DEADNOW=$(sqlq "SELECT dead FROM game_world_entity WHERE guid = $GINGER" | grep -c true)
  assert_eq "reclaim: no longer dead" "$DEADNOW" "0"
  # 50% res + up to a few out-of-combat regen ticks between the reclaim and this read
  assert_ge "reclaim: resurrected at ~50% health (>= half)" "${HP:-0}" "$(( ${MAXHP:-200} / 2 ))"
  assert_lt "reclaim: resurrected at ~50% health (< 3/4)" "${HP:-999}" "$(( ${MAXHP:-0} * 3 / 4 ))"
  assert_eq "reclaim: corpse reclaim gives NO rez sickness (any level)" \
    "$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE target_guid = $GINGER AND spell_id = $SICKNESS")" "0"
  if [ -n "$DUR0" ]; then
    assert_lt "death: main-hand durability took the death loss" \
      "$(sql1 "SELECT durability FROM game_item_instance WHERE owner_guid = $GINGER AND slot = 15")" "$DUR0"
  fi
  # combat state clean: no melee rows referencing Ginger either side
  sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = ${WOLF:-0}" >/dev/null
  assert_eq "combat clean: no melee row with Ginger as attacker" \
    "$(sql1 "SELECT COUNT(*) AS n FROM game_melee_attack WHERE attacker_guid = $GINGER")" "0"
  assert_eq "combat clean: no melee row with Ginger as target" \
    "$(sql1 "SELECT COUNT(*) AS n FROM game_melee_attack WHERE target_guid = $GINGER")" "0"
  rm -f "$DEATH_RECLAIMED" # let the wire client exit
else
  echo "[orch] wire client never reclaimed" >&2; FAILED=1
fi
wait "$WIRE"; RC=$?
[ $RC -ne 0 ] && { echo "[orch] wire scenario failed (rc=$RC)"; FAILED=1; }

# ---- rez-sickness LEVEL GATE via the spirit-healer path (shared do_spirit_healer_res core) ----
# The debug res/kill reducers need a LIVE entity — hold a stay session for the two cycles.
stay_start TEST test123 Ginger || FAILED=1
# below the gate: level 10 -> no sickness
scall debug_set_level "$GINGER" 10
scall debug_set_health "$GINGER" 0
scall debug_repop "$GINGER"
scall debug_spirit_healer_res "$GINGER"
assert_eq "spirit res at L10: NO rez sickness (below the L11 gate)" \
  "$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE target_guid = $GINGER AND spell_id = $SICKNESS")" "0"
# at the gate: level 11 -> sickness aura 15007
scall debug_set_level "$GINGER" 11
scall debug_set_health "$GINGER" 0
scall debug_repop "$GINGER"
scall debug_spirit_healer_res "$GINGER"
assert_ge "spirit res at L11: rez sickness (15007) applied" \
  "$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE target_guid = $GINGER AND spell_id = $SICKNESS")" 1
stay_stop

# ---- teardown (asserted) ----
sqlq "DELETE FROM game_aura WHERE target_guid = $GINGER AND spell_id = $SICKNESS" >/dev/null
scall debug_set_level "$GINGER" 10
scall debug_set_health "$GINGER" 100000
sqlq "DELETE FROM game_creature_spawn WHERE guid = ${WOLF:-0}" >/dev/null
sqlq "DELETE FROM game_world_entity WHERE guid = ${WOLF:-0}" >/dev/null
assert_eq "teardown: pad wolf gone" "$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE guid = ${WOLF:-0}")" "0"
sqlq "DELETE FROM game_corpse WHERE guid = $CORPSE" >/dev/null
assert_eq "teardown: no lingering sickness aura" \
  "$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE target_guid = $GINGER AND spell_id = $SICKNESS")" "0"
assert_eq "teardown: corpse row gone" "$(sql1 "SELECT COUNT(*) AS n FROM game_corpse WHERE guid = $CORPSE")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[scenario-death] PASS"; exit 0; else echo "[scenario-death] FAIL"; exit 1; fi
