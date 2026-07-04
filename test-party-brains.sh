#!/usr/bin/env bash
# Playerbots COMBAT BRAINS acceptance (work-item 148) — one real wire session + three role bots:
#   1. SPAWN + KIT: tank/healer/dps bots spawn with role rows, personality rows (role defaults),
#      and their mock-seed kit spells in the durable spellbook.
#   2. PARTY: the wire client invites all three by name; the on_group_invite hook auto-accepts
#      server-side (no bot client exists) — SMSG_GROUP_LIST grows to the full 4-member roster.
#   3. FIGHT (faction rows staged so wolves genuinely aggro):
#      - TANK: wolves open on the squishies; the tank's brain Taunts (355) them off — cast events
#        + threat rows + the wolf's melee row swinging onto the tank.
#      - HEALER: a damaged member gets Renew (139) — cast event + aura row + rising health.
#      - DPS: assists onto the pack — melee row + Fireball (133) cast events.
#   4. PERSONALITY DIVERGENCE: two extra dps bots at the same health, flee_at_pct 95 vs 0 —
#      one breaks off (melee row gone), the other keeps fighting.
#   5. DISBAND: the leader's disband lands SMSG_GROUP_DESTROYED; teardown asserted.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight party-brains

WOLF_ENTRY=51000
PAD_X=-8890.0; PAD_Y=-460.0; PAD_Z=82.0

# ---- staging ----
scall debug_seed_scenario_fixtures || true
scall playerbots_despawn_all || true
purge_entry_rows $WOLF_ENTRY
sqlq "DELETE FROM game_melee_attack" >/dev/null
sqlq "DELETE FROM game_group_member WHERE character_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_group_invite WHERE target_guid = $GINGER" >/dev/null
# Hostility on mock-seed needs staged faction rows (canonical helper; cleared in teardown).
stage_hostility
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z WHERE guid = $GINGER" >/dev/null

# ---- 1. spawn the role trio + kit asserts ----
scall playerbots_spawn_role 1 $PAD_X -463.0 $PAD_Z 0 || step_fail "tank spawn failed"
scall playerbots_spawn_role 1 $PAD_X -457.0 $PAD_Z 1 || step_fail "healer spawn failed"
scall playerbots_spawn_role 1 $PAD_X -454.0 $PAD_Z 2 || step_fail "dps spawn failed"
TANK=$(char_guid Tankbot1); HEAL=$(char_guid Healbot1); DPS=$(char_guid Dpsbot1)
[ -z "$TANK" ] || [ -z "$HEAL" ] || [ -z "$DPS" ] && { echo "[orch] bot guids missing" >&2; exit 1; }
echo "[orch] bots: tank=$TANK heal=$HEAL dps=$DPS"
assert_eq "spawn: three bot rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_bot")" "3"
assert_eq "spawn: three personality rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_personality")" "3"
assert_eq "kit: tank learned Taunt 355" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_spell WHERE character_guid = $TANK AND spell_id = 355")" "1"
assert_eq "kit: healer learned Renew 139" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_spell WHERE character_guid = $HEAL AND spell_id = 139")" "1"
assert_eq "kit: dps learned Fireball 133" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_spell WHERE character_guid = $DPS AND spell_id = 133")" "1"
H_AT=$(sql1 "SELECT heal_at_pct FROM pkg_playerbots_personality WHERE character_guid = $HEAL")
assert_eq "personality: healer role default heal_at_pct=80" "${H_AT:-0}" "80"

# ---- 2. party up over the wire (auto-accept) ----
# Run-scoped hold path (work-item 161): defined ONCE here, passed as the party-bots hold arg.
HOLD=/tmp/ws_party_bots_$$
rm -f "$HOLD" "$HOLD.ingroup"
timeout 400 "$WC" TEST test123 Ginger party-bots "$HOLD" Tankbot1 Healbot1 Dpsbot1 >/tmp/ws_party_bots.log 2>&1 &
LEADER=$!
for _ in $(seq 1 40); do [ -f "$HOLD.ingroup" ] && break; sleep 1; done
if [ -f "$HOLD.ingroup" ]; then
  step_ok "wire: full 4-member roster over SMSG_GROUP_LIST (bots auto-accepted)"
else
  step_fail "wire: party never formed"; tail -3 /tmp/ws_party_bots.log
fi
assert_eq "sql: four member rows" "$(sql1 "SELECT COUNT(*) AS n FROM game_group_member")" "4"

# ---- 3. the fight ----
# Wolves spawn at the HEALER (nearest player wins proximity aggro), the tank stands ~6yd off —
# so the taunt has real work to do. Health-keeper tops the party so L1 bots survive the pack.
scall debug_spawn_at_feet "$HEAL" $WOLF_ENTRY 2
W1=$(sqlq "SELECT guid FROM game_world_entity WHERE entry = $WOLF_ENTRY" | grep -oE '[0-9]{15,}' | sort -n | tail -1)
scall debug_spawn_at_feet "$DPS" $WOLF_ENTRY 2
W2=$(sqlq "SELECT guid FROM game_world_entity WHERE entry = $WOLF_ENTRY" | grep -oE '[0-9]{15,}' | sort -n | tail -1)
echo "[orch] wolves: $W1 $W2"
CAST_BASE_355=$(sql1 "SELECT COUNT(*) AS n FROM game_spell_cast_event WHERE spell_id = 355")
CAST_BASE_133=$(sql1 "SELECT COUNT(*) AS n FROM game_spell_cast_event WHERE spell_id = 133")
TAUNTED=0; DPS_CASTS=0; TANK_TOP=0; ONTANK_SEEN=0
for i in $(seq 1 20); do
  # keeper: the party stays alive through the pack (bots are L1) AND the wolves stay alive
  # through the party (a Fireball rotation one-shots half a wolf — the asserts need live,
  # swinging wolves, not corpses)
  # top to FULL (clamped to max): a partial top-up left the whole party under the healer's 80%
  # threshold, so it perpetually renewed everyone and drowned the healer phase's clean signal
  for M in "$TANK" "$HEAL" "$DPS" "$GINGER"; do scall debug_set_health "$M" 10000; done
  for W in "$W1" "$W2"; do scall debug_set_health "$W" 10000; done
  sleep 1
  C355=$(sql1 "SELECT COUNT(*) AS n FROM game_spell_cast_event WHERE spell_id = 355")
  C133=$(sql1 "SELECT COUNT(*) AS n FROM game_spell_cast_event WHERE spell_id = 133")
  [ "${C355:-0}" -gt "${CAST_BASE_355:-0}" ] && TAUNTED=1
  [ "${C133:-0}" -gt "${CAST_BASE_133:-0}" ] && DPS_CASTS=1
  # taunt-yank evidence: at SOME sampled instant the tank is the TOP threat source on a wolf
  # (each taunt sets it to table-max+1; the 155 forced-target window then PINS the wolf — the
  # hold itself is asserted separately below)
  for W in "$W1" "$W2"; do
    TOPSRC=$(sqlq "SELECT source_guid, threat FROM game_threat WHERE creature_guid = $W" | sed -n '3,9p' | sort -t'|' -k2 -n | tail -1 | awk -F'|' '{gsub(/ /,"",$1); print $1}')
    [ "${TOPSRC:-x}" = "$TANK" ] && TANK_TOP=1
  done
  ONTANK=$(sql1 "SELECT COUNT(*) AS n FROM game_melee_attack WHERE target_guid = $TANK")
  [ "${ONTANK:-0}" -ge 1 ] && ONTANK_SEEN=1
  if [ "$TAUNTED" = 1 ] && [ "$DPS_CASTS" = 1 ] && [ "$TANK_TOP" = 1 ]; then break; fi
done
[ "$TAUNTED" = 1 ] && step_ok "tank: Taunt 355 cast events fired" || step_fail "tank: no Taunt casts"
assert_ge "tank: threat rows name the tank as a source" "$(sql1 "SELECT COUNT(*) AS n FROM game_threat WHERE source_guid = $TANK")" 1
[ "$TANK_TOP" = 1 ] && step_ok "tank: taunt yank observed (tank topped a wolf's threat table)" || step_fail "tank: never became top threat on a wolf"
# 155: the taunt FORCED-TARGET window (3s) pins the taunted wolf on the tank regardless of the
# dps out-threatening it — sample the melee table on a tight cadence and demand a >=3-sample
# consecutive on-tank run (the pre-155 threat-top alone never held longer than a retarget tick).
[ "$ONTANK_SEEN" = 1 ] && step_ok "tank: a wolf's melee row swung onto the tank" || step_fail "tank: no wolf melee row ever on the tank"
RUN=0; BEST=0
for i in $(seq 1 25); do
  ON=$(sql1 "SELECT COUNT(*) AS n FROM game_melee_attack WHERE target_guid = $TANK")
  if [ "${ON:-0}" -ge 1 ]; then RUN=$((RUN+1)); [ $RUN -gt $BEST ] && BEST=$RUN; else RUN=0; fi
  [ $BEST -ge 3 ] && break
  for M in "$TANK" "$HEAL" "$DPS" "$GINGER"; do scall debug_set_health "$M" 10000; done
  for W in "$W1" "$W2"; do scall debug_set_health "$W" 10000; done
done
assert_ge "tank: taunt window HELD the wolf on the tank (consecutive on-tank samples)" "$BEST" 3
[ "$DPS_CASTS" = 1 ] && step_ok "dps: Fireball 133 rotation fired" || step_fail "dps: no Fireball casts"
assert_ge "dps: melee assist row armed" "$(sql1 "SELECT COUNT(*) AS n FROM game_melee_attack WHERE attacker_guid = $DPS")" 1

# HEALER: hurt the dps bot below the 80% threshold and watch Renew land + tick it back up.
# Any stale Renew from the fight (a wolf-damage dip) is waited out first — the fresh cast on the
# watched member is the assertion. Tank/healer stay topped so the pack can't kill them meanwhile.
for i in $(seq 1 20); do
  AURA=$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE target_guid = $DPS AND spell_id = 139")
  [ "${AURA:-0}" = "0" ] && break
  scall debug_set_health "$TANK" 10000; scall debug_set_health "$HEAL" 10000
  sleep 1
done
scall debug_set_health "$DPS" 15
CAST_BASE_139=$(sql1 "SELECT COUNT(*) AS n FROM game_spell_cast_event WHERE spell_id = 139")
HEALED=0
for i in $(seq 1 10); do
  scall debug_set_health "$TANK" 10000; scall debug_set_health "$HEAL" 10000
  sleep 1
  C139=$(sql1 "SELECT COUNT(*) AS n FROM game_spell_cast_event WHERE spell_id = 139")
  AURA=$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE target_guid = $DPS AND spell_id = 139")
  if [ "${C139:-0}" -gt "${CAST_BASE_139:-0}" ] && [ "${AURA:-0}" -ge 1 ]; then HEALED=1; break; fi
done
[ "$HEALED" = 1 ] && step_ok "healer: Renew cast on the damaged member (cast event + aura row)" || step_fail "healer: no Renew on the hurt member"
# The HoT ticks 8/1s; retry the rising-health sample a few times in case a stray wolf swing lands
# between samples (the taunt phase normally has the pack on the tank by now).
HP_UP=0
for i in $(seq 1 6); do
  HP0=$(sql1 "SELECT health FROM game_world_entity WHERE guid = $DPS")
  sleep 2
  HP1=$(sql1 "SELECT health FROM game_world_entity WHERE guid = $DPS")
  if [ "${HP1:-0}" -gt "${HP0:-0}" ]; then HP_UP=1; break; fi
done
[ "$HP_UP" = 1 ] && step_ok "healer: the HoT is ticking the member's health up (${HP0:-?} -> ${HP1:-?})" || step_fail "healer: member health never rose under the HoT"

# ---- 4. personality divergence (two ungrouped dps bots, same damage, different flee) ----
scall playerbots_spawn_role 2 -8930.0 -560.0 $PAD_Z 2
B_STAY=$(char_guid Dpsbot2); B_FLEE=$(char_guid Dpsbot3)
sqlq "UPDATE pkg_playerbots_personality SET flee_at_pct = 0 WHERE character_guid = $B_STAY" >/dev/null
sqlq "UPDATE pkg_playerbots_personality SET flee_at_pct = 95 WHERE character_guid = $B_FLEE" >/dev/null
# Engage both on GINGER (~108yd away): a PLAYER target never retaliates, never chases, and never
# dies to the crossfire (a parked WOLF did all three — the retaliation pass arms off the row
# alone, the chase pass closes the distance, and the Fireball rotation killed it, wiping both
# rows). At this range the swing tick is range-gated and the brain's re-arm attempt is rejected
# by the friendly-faction gate — the rows are in perfect stasis until ONLY the flee brain acts.
scall debug_set_health "$B_STAY" 20
scall debug_set_health "$B_FLEE" 20
scall debug_engage "$B_STAY" "$GINGER"
scall debug_engage "$B_FLEE" "$GINGER"
sleep 3 # >= one brain tick
STAY_ROW=$(sql1 "SELECT COUNT(*) AS n FROM game_melee_attack WHERE attacker_guid = $B_STAY")
FLEE_ROW=$(sql1 "SELECT COUNT(*) AS n FROM game_melee_attack WHERE attacker_guid = $B_FLEE")
assert_eq "divergence: flee_at_pct=0 bot kept fighting" "${STAY_ROW:-0}" "1"
assert_eq "divergence: flee_at_pct=95 bot broke off" "${FLEE_ROW:-0}" "0"

# ---- 5. disband ----
touch "$HOLD"
wait "$LEADER"; RC_L=$?
[ $RC_L -eq 0 ] && step_ok "wire: leader flow green (roster -> hold -> disband -> DESTROYED)" || { step_fail "leader wire flow rc=$RC_L"; tail -4 /tmp/ws_party_bots.log; }
# A 4-member group SURVIVES the leader leaving — leadership transfers to a bot (the leaver still
# gets SMSG_GROUP_DESTROYED for its own party UI). The bot party dissolves when the despawn
# sweep removes its members below 2.
assert_eq "sql: the bot party lives on after the leader leaves (3 members)" "$(sql1 "SELECT COUNT(*) AS n FROM game_group_member")" "3"
NEWLEAD=$(sql1 "SELECT leader_guid FROM game_group")
case "$NEWLEAD" in "$TANK"|"$HEAL"|"$DPS") step_ok "sql: leadership transferred to a bot ($NEWLEAD)";; *) step_fail "sql: leader after leave is $NEWLEAD (want a bot)";; esac

# ---- teardown (asserted) ----
scall playerbots_despawn_all || true
assert_eq "teardown: bot party dissolved by the despawn sweep" "$(sql1 "SELECT COUNT(*) AS n FROM game_group")" "0"
purge_entry_rows $WOLF_ENTRY
sqlq "DELETE FROM game_melee_attack" >/dev/null
clear_hostility
rm -f "$HOLD" "$HOLD.ingroup"
assert_eq "teardown: zero bot rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_bot")" "0"
assert_eq "teardown: zero personality rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_personality")" "0"
assert_eq "teardown: zero faction rows" "$(sql1 "SELECT COUNT(*) AS n FROM game_faction_template")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[party-brains] PASS"; exit 0; else echo "[party-brains] FAIL"; exit 1; fi
