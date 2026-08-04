#!/usr/bin/env bash
# Party/group acceptance (work-item 066) — two real wire sessions, all headless:
#   1. INVITE/ACCEPT: leader invites by name (SMSG_PARTY_COMMAND_RESULT Success); target receives
#      SMSG_GROUP_INVITE, accepts; BOTH receive SMSG_GROUP_LIST reflecting the two-member party
#      (leader guid + peer name asserted wire-side; group/member rows asserted via sql).
#   2. XP SPLIT: with both members in range, a kill by the leader awards split XP to both
#      (each 1/n of its own level-based award); a kill with the member OUT of the 74yd group
#      range pays the leader alone (the range gate).
#   3. SHARED QUEST CREDIT: both members hold the fixture kill quest; the leader's kill advances
#      BOTH quest logs (in range only).
#   4. LEAVE/DISBAND: the leader's CMSG_GROUP_DISBAND dissolves the 2-man group — both sessions
#      receive SMSG_GROUP_DESTROYED; zero group/member rows remain.
#   5. DECLINE: a fresh invite declined by the target lands SMSG_GROUP_DECLINE at the inviter.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"
scenario_preflight group
DFS=$(char_guid dfsdfsd)
[ -z "$DFS" ] && timeout 60 "$WC" TEST2 dfsdfsd logout >/dev/null 2>&1 && DFS=$(char_guid dfsdfsd)
[ -z "$DFS" ] && { echo "[orch] no dfsdfsd character" >&2; exit 1; }

QUEST=50900; WOLF_ENTRY=51000
PAD_X=-8905.0; PAD_Y=-440.0; PAD_Z=82.0
FAR_X=-8905.0; FAR_Y=-640.0    # ~200yd: outside the 74yd group-XP range, same map

# repeatability: no stale groups/invites/quest rows; co-locate both chars on the pad; equal level
# (L2: the fixture wolf is L1 — green to both, so each side's share is deterministic base 50 / 2)
sqlq "DELETE FROM game_group_member WHERE character_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_group_member WHERE character_guid = $DFS" >/dev/null
sqlq "DELETE FROM game_group_invite WHERE target_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_group_invite WHERE target_guid = $DFS" >/dev/null
sqlq "DELETE FROM game_character_quest WHERE character_guid = $GINGER AND quest_entry = $QUEST" >/dev/null
sqlq "DELETE FROM game_character_quest WHERE character_guid = $DFS AND quest_entry = $QUEST" >/dev/null
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z WHERE guid = $GINGER" >/dev/null
sqlq "UPDATE game_character SET x = -8902.0, y = -440.0, z = $PAD_Z WHERE guid = $DFS" >/dev/null

# Run-scoped handshake paths (work-item 161): defined ONCE here, passed as wire-client args.
HOLD_L=/tmp/ws_group_leader_$$; HOLD_J=/tmp/ws_group_join_$$
rm -f "$HOLD_L" "$HOLD_J" "$HOLD_L.ingroup" "$HOLD_J.ingroup"

# ---- 1. invite/accept over two live sessions ----
timeout 300 "$WC" TEST2 dfsdfsd group-join "$HOLD_J" >/tmp/ws_group_join.log 2>&1 &
JOINER=$!
sleep 3
timeout 300 "$WC" TEST Ginger group-leader dfsdfsd "$HOLD_L" >/tmp/ws_group_leader.log 2>&1 &
LEADER=$!
# both sides must signal in-group.
# Kept hand-rolled: ONE 40s deadline shared by BOTH files (chained wait_for_file calls would
# quietly double the budget to 80s and mask a join-latency regression).
for _ in $(seq 1 40); do [ -f "$HOLD_L.ingroup" ] && [ -f "$HOLD_J.ingroup" ] && break; sleep 1; done
if [ -f "$HOLD_L.ingroup" ] && [ -f "$HOLD_J.ingroup" ]; then
  step_ok "wire: both sessions decoded SMSG_GROUP_LIST (two-member party formed)"
else
  step_fail "wire: party never formed (leader=$([ -f $HOLD_L.ingroup ] && echo ok || echo no) joiner=$([ -f $HOLD_J.ingroup ] && echo ok || echo no))"
  tail -3 /tmp/ws_group_leader.log /tmp/ws_group_join.log
fi
assert_eq "sql: one group row" "$(sql1 "SELECT COUNT(*) AS n FROM game_group")" "1"
assert_eq "sql: two member rows" "$(sql1 "SELECT COUNT(*) AS n FROM game_group_member")" "2"
assert_eq "sql: leader is the inviter" "$(sql1 "SELECT leader_guid FROM game_group")" "$GINGER"

# ---- 2+3. XP split + shared quest credit (both sessions still live and in range) ----
scall debug_seed_scenario_fixtures || true
scall debug_set_level "$GINGER" 2
scall debug_set_level "$DFS" 2
# zero any rested-XP pool: a rested member's kill share is DOUBLED (vanilla), which would skew the
# exact 25/25 split assertions on a long-lived node
sqlq "UPDATE game_character SET rested_xp = 0 WHERE guid = $GINGER" >/dev/null
sqlq "UPDATE game_character SET rested_xp = 0 WHERE guid = $DFS" >/dev/null
scall debug_set_xp_rate 1.0
# …and the OTHER multiplier. The GM `.xprate` dot-command (module/src/gm.rs) writes a WORLD
# singleton that `grant_xp` applies on top of `game_config.xp_rate`, so an operator playtesting at
# `.xprate 3` triples every award realm-wide — including this test's, which then reads 75 where it
# wants 25 and looks exactly like an XP-split bug. Scale the expectations to whatever the realm is
# actually set to rather than overwriting an operator's live setting (it is not this test's to
# clobber, and a mid-run abort would leave it clobbered).
XPBP=$(sql1 "SELECT value FROM game_world_config WHERE key = 'xprate'"); XPBP=${XPBP:-10000}
[ "$XPBP" = "10000" ] || echo "[orch] note: GM .xprate is ${XPBP}bp ($((XPBP/100))% ) — scaling XP expectations"
# The module multiplies at grant time, AFTER the group split, with integer truncation — mirror that
# order here or the rounding disagrees.
xps(){ echo $(( $1 * XPBP / 10000 )); }
scall debug_grant_quest "$GINGER" $QUEST
scall debug_grant_quest "$DFS" $QUEST
XP_G0=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $GINGER")
XP_D0=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $DFS")
WOLF=$(spawn_at "$GINGER" $WOLF_ENTRY 3)
if [ -z "$WOLF" ]; then step_fail "wolf spawn failed"; fi
scall debug_kill_creature "$GINGER" "${WOLF:-0}" || step_fail "kill reducer failed"
sleep 1
XP_G1=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $GINGER")
XP_D1=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $DFS")
# wolf L1 at player L2: base 5*1+45 = 50, split 2 ways -> 25 each
assert_eq "xp split: leader got $(xps 25) (50 base / 2 members, .xprate ${XPBP}bp)" "$(( ${XP_G1:-0} - ${XP_G0:-0} ))" "$(xps 25)"
assert_eq "xp split: member got $(xps 25) (in range)" "$(( ${XP_D1:-0} - ${XP_D0:-0} ))" "$(xps 25)"
Q_G=$(sqlq "SELECT counts FROM game_character_quest WHERE character_guid = $GINGER AND quest_entry = $QUEST" | grep -oE '[0-9]+' | head -1)
Q_D=$(sqlq "SELECT counts FROM game_character_quest WHERE character_guid = $DFS AND quest_entry = $QUEST" | grep -oE '[0-9]+' | head -1)
assert_eq "quest credit: leader's kill count advanced" "${Q_G:-0}" "1"
assert_eq "quest credit: member's kill count advanced (shared)" "${Q_D:-0}" "1"

# range gate: move the member ~200yd out, kill again — only the leader gains
scall debug_teleport "$DFS" 0 $FAR_X $FAR_Y $PAD_Z 0
WOLF2=$(spawn_at "$GINGER" $WOLF_ENTRY 4)
scall debug_kill_creature "$GINGER" "${WOLF2:-0}" || step_fail "second kill reducer failed"
sleep 1
XP_G2=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $GINGER")
XP_D2=$(sql1 "SELECT xp FROM game_world_entity WHERE guid = $DFS")
assert_eq "range gate: out-of-range member gained nothing" "$(( ${XP_D2:-0} - ${XP_D1:-0} ))" "0"
assert_eq "range gate: leader took the full $(xps 50) solo" "$(( ${XP_G2:-0} - ${XP_G1:-0} ))" "$(xps 50)"
Q_D2=$(sqlq "SELECT counts FROM game_character_quest WHERE character_guid = $DFS AND quest_entry = $QUEST" | grep -oE '[0-9]+' | head -1)
assert_eq "range gate: out-of-range member got no quest credit" "${Q_D2:-0}" "1"

# ---- 4. leader disbands: both sessions must see SMSG_GROUP_DESTROYED ----
touch "$HOLD_L"; touch "$HOLD_J"
wait "$LEADER"; RC_L=$?
wait "$JOINER"; RC_J=$?
[ $RC_L -eq 0 ] && step_ok "wire: leader flow green (disband -> SMSG_GROUP_DESTROYED)" || { step_fail "leader wire flow rc=$RC_L"; tail -3 /tmp/ws_group_leader.log; }
[ $RC_J -eq 0 ] && step_ok "wire: joiner flow green (SMSG_GROUP_DESTROYED on disband)" || { step_fail "joiner wire flow rc=$RC_J"; tail -3 /tmp/ws_group_join.log; }
assert_eq "sql: zero group rows after disband" "$(sql1 "SELECT COUNT(*) AS n FROM game_group")" "0"
assert_eq "sql: zero member rows after disband" "$(sql1 "SELECT COUNT(*) AS n FROM game_group_member")" "0"

# ---- 5. decline flow ----
sleep 3 # settle the relogins
timeout 120 "$WC" TEST2 dfsdfsd group-decline >/tmp/ws_group_decline.log 2>&1 &
DECL=$!
sleep 3
if timeout 120 "$WC" TEST Ginger group-invite-expect-decline dfsdfsd >/tmp/ws_group_declflow.log 2>&1; then
  step_ok "wire: decline flow (invite acked, SMSG_GROUP_DECLINE received)"
else
  step_fail "wire: decline flow failed ($(tail -1 /tmp/ws_group_declflow.log))"
fi
wait "$DECL" 2>/dev/null

# ---- teardown (asserted) ----
for W in "$WOLF" "$WOLF2"; do
  [ -n "$W" ] || continue
  sqlq "DELETE FROM game_creature_spawn WHERE guid = $W" >/dev/null
  sqlq "DELETE FROM game_world_entity WHERE guid = $W" >/dev/null
  sqlq "DELETE FROM game_corpse_loot WHERE corpse_guid = $W" >/dev/null
done
sqlq "DELETE FROM game_character_quest WHERE character_guid = $GINGER AND quest_entry = $QUEST" >/dev/null
sqlq "DELETE FROM game_character_quest WHERE character_guid = $DFS AND quest_entry = $QUEST" >/dev/null
scall debug_set_level "$GINGER" 10
rm -f "$HOLD_L" "$HOLD_J" "$HOLD_L.ingroup" "$HOLD_J.ingroup"
assert_eq "teardown: no lingering invites" "$(sql1 "SELECT COUNT(*) AS n FROM game_group_invite")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[group] PASS"; exit 0; else echo "[group] FAIL"; exit 1; fi
