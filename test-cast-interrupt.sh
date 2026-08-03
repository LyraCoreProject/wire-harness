#!/usr/bin/env bash
# Cast-INTERRUPT wire test (headless). The completion sibling is test-cast-flow.sh; this one verifies the
# OTHER outcome: a hostile mob in MELEE range hits the caster mid-cast, so the gateway relays
# SMSG_SPELL_FAILURE (the cast-interrupt-on-damage signal, b4628b2 + the relay) and NO SMSG_SPELL_GO.
# This is the e2e proof of cast-interrupt that the spacetime-call guid-mangling blocked earlier.
#
# Prereqs: local STDB node + gateway up, TEST provisioned, Warlock "Ginger" (guid 5).
# Usage: tools/wire-client/test-cast-interrupt.sh [spell_id] [mob_entry]
set -uo pipefail
cd "$(dirname "$0")/../.."

source tools/wire-client/scenario-lib.sh
SPELL="${1:-686}"   # 686 = Shadow Bolt (1.7s timed)
ENTRY="${2:-103}"   # 103 = Garrick Padfoot (hostile Defias) — melee attacker
CHAR="Ginger"
# ensure_ginger_home (issue #213), not a bare char_guid: Ginger is the shared long-lived fixture and
# is not guaranteed to still be on spacetime-core (region-boundary logins can transfer her live row
# to another shard; a stale duplicate from an old create-on-miss fallback can also shadow her at
# login) — self-heals both before handing back a guid.
CGUID=$(ensure_ginger_home "$CHAR")
[ -z "$CGUID" ] && { echo "[test] character '$CHAR' not found in game_character" >&2; exit 1; }

cargo build -q -p wire-client || exit 1
TGT="$(mktemp -u /tmp/wc_target_XXXXXX)"; rm -f "$TGT"
# Run-scoped side-channel for the spawned mob's guid (work-item 161: no shared /tmp literal).
MOB_FILE=/tmp/wc_mobguid_$$
MOBGUID=""

orchestrate() {
  local GX="" GY="" MOB=""
  # Self-heal: purge STALE Padfoots from prior crashed/pre-fix runs (spawn rows resurrect them),
  # then ORPHAN entities whose spawn row is already gone (a spawn-keyed loop misses them).
  for SG in $(spacetime sql "$DB" "SELECT guid FROM game_creature_spawn WHERE entry = $ENTRY" 2>/dev/null | grep -oE '[0-9]{15,}'); do
    spacetime sql "$DB" "DELETE FROM game_creature_spawn WHERE guid = $SG" >/dev/null 2>&1
    spacetime sql "$DB" "DELETE FROM game_world_entity WHERE guid = $SG" >/dev/null 2>&1
  done
  for SG in $(spacetime sql "$DB" "SELECT guid FROM game_world_entity WHERE entry = $ENTRY" 2>/dev/null | grep -oE '[0-9]{15,}'); do
    spacetime sql "$DB" "DELETE FROM game_melee_attack WHERE attacker_guid = $SG" >/dev/null 2>&1
    spacetime sql "$DB" "DELETE FROM game_world_entity WHERE guid = $SG" >/dev/null 2>&1
  done
  # Park on probed-clear ground first (debug_nav_leg has_los=true): in-suite Ginger inherits the
  # previous test's position, which may be nav-obstructed (the 243 LoS gate silently eats swings).
  spacetime call "$DB" -- debug_teleport $CGUID 0 -8960.0 -440.0 81.0 0 >/dev/null 2>&1
  for _ in $(seq 1 30); do
    GX=$(spacetime sql "$DB" "SELECT x FROM game_world_entity WHERE guid=$CGUID" 2>&1 | grep -oE '\-?[0-9]+(\.[0-9]+)?' | tail -1)
    [ -n "$GX" ] && break; sleep 1
  done
  [ -z "$GX" ] && { echo "[orch] $CHAR never went live" >&2; return 1; }
  GY=$(spacetime sql "$DB" "SELECT y FROM game_world_entity WHERE guid=$CGUID" 2>&1 | grep -oE '\-?[0-9]+(\.[0-9]+)?' | tail -1)
  spacetime call "$DB" debug_spawn_at_feet $CGUID $ENTRY 1 >/dev/null 2>&1   # 1yd = immediate melee
  spacetime call "$DB" debug_set_health $CGUID 100000 >/dev/null 2>&1
  MOB=$(spacetime sql "$DB" "SELECT guid, x, y FROM game_world_entity WHERE entry=$ENTRY AND owner_guid=0" 2>&1 \
        | awk -F'|' -v gx="$GX" -v gy="$GY" '$1 ~ /[0-9]/ {g=$1;x=$2+0;y=$3+0;dx=x-gx;dy=y-gy;d=dx*dx+dy*dy; if(best==""||d<best){best=d;bg=g}} END{gsub(/ /,"",bg);print bg}')
  echo "$MOB" > "$MOB_FILE"
  # DETERMINISTIC swing source (2026-07-16 flake fix): arm the mob's engagement directly instead of
  # waiting on the aggro sense tick (up to 4s of jitter that sometimes left ZERO swings inside the
  # 1.7s cast window even across the probe's 3 retry windows). debug_engage's fresh row swings on
  # the next melee tick and every ~2s after — every cast window sees a swing attempt.
  [ -n "$MOB" ] && spacetime call "$DB" -- debug_engage "$MOB" "$CGUID" >/dev/null 2>&1
  sleep 2
  spacetime call "$DB" debug_set_health $CGUID 100000 >/dev/null 2>&1   # re-top after the first swings
  echo "[orch] interrupt target mob=$MOB (melee range, swinging)" >&2
  echo "$MOB" > "$TGT"
  # DETERMINISTIC pushback source (testing-hardening): the mob's swings are a per-window lottery
  # (miss/dodge or between-window timing left ZERO landed hits across 3 windows ~half the runs).
  # debug_apply_damage routes through the REAL damage path (break_auras_on_damage -> pushback), so
  # two pokes inside the first 1.7s cast window guarantee the slide the probe asserts. The mob
  # stays for the realistic swing traffic on top.
  sleep 1
  spacetime call "$DB" -- debug_apply_damage $CGUID 3 0 >/dev/null 2>&1
  sleep 1
  spacetime call "$DB" -- debug_apply_damage $CGUID 3 0 >/dev/null 2>&1
}

orchestrate &
ORCH=$!
WIRE_EXPECT_INTERRUPT=1 WIRE_TARGET_FILE="$TGT" timeout 120 cargo run -q -p wire-client -- TEST test123 "$CHAR" "$SPELL"
RC=$?
wait "$ORCH" 2>/dev/null || true
rm -f "$TGT"
# cleanup the debug-spawned mob — ENTITY AND SPAWN ROW (danger-zones: debug_spawn_at_feet writes a
# game_creature_spawn row; deleting only the entity lets the respawn pass RESURRECT the mob, and the
# strays accumulated across runs — two camped the train pad and beat Ginger's casts to death).
MOBGUID=$(cat "$MOB_FILE" 2>/dev/null || echo "")
if [ -n "$MOBGUID" ]; then
  spacetime sql "$DB" "DELETE FROM game_creature_spawn WHERE guid = $MOBGUID" >/dev/null 2>&1 || true
  spacetime sql "$DB" "DELETE FROM game_world_entity WHERE guid = $MOBGUID" >/dev/null 2>&1 || true
fi
rm -f "$MOB_FILE"
exit $RC
