#!/usr/bin/env bash
# PET CONTROL (warlock pet command bar, CMSG_PET_ACTION): summon an Imp on the warlock test char, then
# drive each bar action via debug_pet_command (the ctx.sender-free counterpart to the pet_command
# reducer) and assert BOTH the stored game_pet_command state AND that pass_pet honors it — ATTACK
# engages the commanded target, DEFENSIVE assists the owner's target, PASSIVE suppresses, FOLLOW clears,
# DISMISS despawns. Runs on the empty combat pad (270) so no stray interferes with the pet's melee rows.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight pet-control

# Ginger is the shared warlock fixture (CLASS 9) — which is where the old hardcoded `GINGER=9` came
# from: the class id was written into the GUID. Resolve by name like test-cast-flow.sh does; the guid
# has been 3, 9 and 13, and a stale one makes every step fail against a character that doesn't exist.
GINGER=$(char_guid Ginger)
[ -n "$GINGER" ] || { echo "[pet-control] no Ginger character on spacetime-core" >&2; exit 1; }
WOLF_ENTRY=51000
PAD_X=-9235; PAD_Y=-465; PAD_Z=92 # empty pad
# packed CMSG_PET_ACTION.data (flag<<24 | id): command flag 0x07, react flag 0x06
STAY=117440512; FOLLOW=117440513; ATTACK=117440514; DISMISS=117440515
PASSIVE=100663296; DEFENSIVE=100663297; AGGRESSIVE=100663298

FAILED=0
ok(){ echo "  OK   $1"; }
chk(){ # chk "label" actual expected
  if [ "$2" = "$3" ]; then ok "$1 ($2)"; else echo "  FAIL $1: got '$2' want '$3'"; FAILED=1; fi
}
# pass_pet runs on the ~4s sense tick, which can slide under full-suite load (270) — POLL for the
# expected value up to ~18s instead of a fixed sleep, so a positive-behaviour assert never flakes.
poll_chk(){ # poll_chk "label" "sql" expected
  local got=""
  for _ in $(seq 1 18); do got=$(sql1 "$2"); [ "$got" = "$3" ] && break; sleep 1; done
  chk "$1" "$got" "$3"
}
settle(){ sleep 10; } # for a NEGATIVE assert: wait ≥2 sense ticks so pass_pet HAS run, then assert it didn't act

# --- stage: live warlock + a summoned Imp on the empty pad ---
scall debug_set_level "$GINGER" 10
stay_start TEST test123 Ginger || exit 1
scall debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0
scall debug_set_health "$GINGER" 100000
purge_creatures_near "$PAD_X" "$PAD_Y" 40
scall debug_force_cast "$GINGER" 688 # Summon Imp (E_SUMMON_PET)
sleep 2
PET=$(sql1 "SELECT guid FROM game_world_entity WHERE owner_guid = $GINGER")
if [ -z "$PET" ]; then echo "[pet-control] FAIL: pet never summoned"; stay_stop; exit 1; fi
ok "Imp summoned (pet=$PET, owner=$GINGER)"
WOLF=$(spawn_at "$GINGER" $WOLF_ENTRY 4)
[ -n "$WOLF" ] && ok "hostile spawned (wolf=$WOLF)" || { echo "[pet-control] FAIL: wolf spawn"; FAILED=1; }

pet_melee_target(){ sql1 "SELECT target_guid FROM game_melee_attack WHERE attacker_guid = $PET"; }
pet_has_melee(){ sqlq "SELECT attacker_guid FROM game_melee_attack WHERE attacker_guid = $PET" | grep -c "$PET"; }

# --- 1) ATTACK <wolf>: state + pass_pet engages the commanded target ---
scall debug_pet_command "$GINGER" $ATTACK "$WOLF"
chk "ATTACK: command=Attack(2)" "$(sql1 "SELECT command FROM game_pet_command WHERE owner_guid = $GINGER")" "2"
chk "ATTACK: command_target=wolf" "$(sql1 "SELECT command_target FROM game_pet_command WHERE owner_guid = $GINGER")" "$WOLF"
poll_chk "ATTACK: pass_pet armed pet→wolf melee" "SELECT target_guid FROM game_melee_attack WHERE attacker_guid = $PET" "$WOLF"

# --- 2) FOLLOW: clears the attack, pet disengages (owner idle, defensive default, nothing to assist) ---
scall debug_pet_command "$GINGER" $FOLLOW 0
chk "FOLLOW: command=Follow(1)" "$(sql1 "SELECT command FROM game_pet_command WHERE owner_guid = $GINGER")" "1"
sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = $PET" >/dev/null # clear the prior engage; prove pass_pet doesn't re-arm
settle
chk "FOLLOW: pet stays disengaged (owner idle)" "$(pet_has_melee)" "0"

# --- 3) DEFENSIVE assist: owner engages the wolf → the pet assists the owner's target ---
scall debug_pet_command "$GINGER" $DEFENSIVE 0
scall debug_engage "$GINGER" "$WOLF" # owner now "in combat" with the wolf (arms owner→wolf melee)
poll_chk "DEFENSIVE: pet assists the owner's target" "SELECT target_guid FROM game_melee_attack WHERE attacker_guid = $PET" "$WOLF"

# --- 4) PASSIVE suppress: same owner-combat, but a passive pet must NOT engage (step 3 just proved the tick fires) ---
sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = $PET" >/dev/null
scall debug_pet_command "$GINGER" $PASSIVE 0
chk "PASSIVE: react=Passive(0)" "$(sql1 "SELECT react FROM game_pet_command WHERE owner_guid = $GINGER")" "0"
settle
chk "PASSIVE: pet does NOT auto-engage" "$(pet_has_melee)" "0"
sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = $GINGER" >/dev/null # end owner combat

# --- 5) STAY state ---
scall debug_pet_command "$GINGER" $STAY 0
chk "STAY: command=Stay(0)" "$(sql1 "SELECT command FROM game_pet_command WHERE owner_guid = $GINGER")" "0"

# --- 6) DISMISS: despawns the pet AND clears the command row ---
scall debug_pet_command "$GINGER" $DISMISS 0
sleep 1
chk "DISMISS: pet despawned" "$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE owner_guid = $GINGER")" "0"
chk "DISMISS: command row cleared" "$(sql1 "SELECT COUNT(*) AS n FROM game_pet_command WHERE owner_guid = $GINGER")" "0"

# --- teardown ---
stay_stop
sqlq "DELETE FROM game_creature_spawn WHERE guid = ${WOLF:-0}" >/dev/null
sqlq "DELETE FROM game_world_entity WHERE guid = ${WOLF:-0}" >/dev/null
scall debug_set_level "$GINGER" 2

if [ "$FAILED" -eq 0 ]; then echo "[pet-control] PASS"; exit 0; else echo "[pet-control] FAIL"; exit 1; fi
