#!/usr/bin/env bash
# FOLLOW THE LEADER (276 slice 2) — a player-grouped bot's between-fights job is to BE WITH the
# party: goals suspend (266) and the follow leg + leader-anchored wander replace the old
# drift-back-to-spawn. One wire session (Ginger leads and holds), then the leader is teleported
# ~80yd away twice; the bot must converge to FOLLOW range each time, and must NOT converge after
# the group disbands (the follow is group-scoped, not a leash on Ginger herself).
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"
scenario_preflight bot-follow
ensure_playerbots_package bot-follow

PAD_X=-8930.0; PAD_Y=-250.0; PAD_Z=80.0
HOP1_X=-8990.0; HOP1_Y=-300.0   # ~78yd southwest
HOP2_X=-8920.0; HOP2_Y=-190.0   # ~130yd back northeast

dist_bot_ginger() { # prints integer yd between bot and Ginger
  sqlq "SELECT guid, x, y FROM game_world_entity WHERE guid = $BOT OR guid = $GINGER" | grep -oE '\-?[0-9]+(\.[0-9]+)?' \
    | awk -v bot="$BOT" -v gin="$GINGER" 'NR%3==1{g=$0} NR%3==2{x[g]=$0} NR%3==0{y[g]=$0} END{dx=x[bot]-x[gin]; dy=y[bot]-y[gin]; print int(sqrt(dx*dx+dy*dy))}'
}

# ---- staging: one dps bot + Ginger at the pad, grouped ----
scall debug_seed_scenario_fixtures || true
scall playerbots_despawn_all || true
sqlq "DELETE FROM game_group_member WHERE character_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_group_invite WHERE target_guid = $GINGER" >/dev/null
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z WHERE guid = $GINGER" >/dev/null
scall playerbots_spawn_role 1 $PAD_X $PAD_Y $PAD_Z 2 || { echo "[orch] bot spawn failed" >&2; exit 1; }
BOT=$(char_guid Dpsbot1)
[ -z "$BOT" ] && { echo "[orch] no bot guid" >&2; exit 1; }
scall debug_set_level "$BOT" 10
echo "[orch] bot-follow: bot=$BOT ginger=$GINGER"

HOLD=/tmp/ws_bot_follow_$$
rm -f "$HOLD" "$HOLD.ingroup"
timeout 400 "$WC" TEST Ginger party-bots "$HOLD" Dpsbot1 >/tmp/ws_bot_follow.log 2>&1 &
LEADER=$!
wait_for_file 40 "$HOLD.ingroup"
[ -f "$HOLD.ingroup" ] && step_ok "wire: bot grouped under Ginger" || { step_fail "wire: party never formed"; tail -3 /tmp/ws_bot_follow.log; }

# ---- 1. leader teleports away; the bot runs to it ----
scall debug_teleport "$GINGER" 0 $HOP1_X $HOP1_Y $PAD_Z 0
CONVERGED=0
for i in $(seq 1 30); do
  D=$(dist_bot_ginger)
  [ "${D:-999}" -le 15 ] 2>/dev/null && CONVERGED=1 && break
  sleep 1
done
[ "$CONVERGED" = 1 ] && step_ok "follow: bot converged to the teleported leader (${D}yd)" || step_fail "follow: bot never closed on hop 1 (last ${D:-?}yd)"

# ---- 2. and again, the other way (not a one-shot fluke) ----
scall debug_teleport "$GINGER" 0 $HOP2_X $HOP2_Y $PAD_Z 0
CONVERGED=0
for i in $(seq 1 30); do
  D=$(dist_bot_ginger)
  [ "${D:-999}" -le 15 ] 2>/dev/null && CONVERGED=1 && break
  sleep 1
done
[ "$CONVERGED" = 1 ] && step_ok "follow: bot converged on hop 2 (${D}yd)" || step_fail "follow: bot never closed on hop 2 (last ${D:-?}yd)"

# ---- 3. disband: the follow is group-scoped ----
touch "$HOLD"   # release the wire leader → it disbands and exits
wait "$LEADER" 2>/dev/null
scall debug_teleport "$GINGER" 0 $HOP1_X $HOP1_Y $PAD_Z 0
sleep 12
D=$(dist_bot_ginger)
[ "${D:-0}" -ge 30 ] 2>/dev/null && step_ok "disband: ungrouped bot stayed put (${D}yd away)" || step_fail "disband: bot still shadowing Ginger (${D:-?}yd)"

# ---- teardown ----
scall playerbots_despawn_all || true
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z WHERE guid = $GINGER" >/dev/null
assert_eq "teardown: zero bot rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_bot")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[bot-follow] PASS"; exit 0; else echo "[bot-follow] FAIL"; exit 1; fi
