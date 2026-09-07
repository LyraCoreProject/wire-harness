#!/usr/bin/env bash
# A PLAYER invites a BOT (issue #51) — the single manual test the playerbots exist to support,
# headless: one real wire session invites one spawned bot by name, the bot ANSWERS, and the roster
# the player's own client decoded carries both.
#
# WHY THIS EXISTS ALONGSIDE the four scripts that already invite bots (test-bot-follow.sh,
# test-party-brains.sh, test-class-roles.sh, test-bot-deadmines.sh): all four stayed GREEN through
# the entire #51 outage. A SINGLE-DATABASE gateway answers a bot's invite inside the module — the
# `group_invite` reducer fires `on_group_invite` and `brain.rs`'s `playerbots_auto_accept` accepts in
# the same transaction — and this harness is single-database. The bug only existed on a gateway with
# LYRACORE_REALM_CORE set, where the invite is authoritative on realm-core (whose `pkg_playerbots_bot` is
# empty, so the hook is a no-op) and the answer has to come from the gateway's routing layer.
#
# So the script has two arms:
#   * ALWAYS — invite → join → roster, plus the shard's own membership rows (which on a sharded
#     gateway are the write-through MIRROR the bot's in-world reads use: follow-the-leader, the
#     kill-XP split, /p).
#   * WHEN `REALM_DB` names a realm-core database — the same party asserted on the AUTHORITY. This is
#     the arm that fails without the #51 fix; run it as:
#         REALM_DB=lyracore-realm bash adapters/lyracore/test-bot-invite.sh
#     against a gateway started with LYRACORE_REALM_CORE=$REALM_DB (the multi-database shape
#     test-transfer-crash-matrix.sh documents for its own second database).
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"
scenario_preflight bot-invite
ensure_playerbots_package bot-invite

# package-installed gate (the drop-in may legitimately be absent) — same shape as test-playerbots.sh
if ! sqlq "SELECT id FROM pkg_playerbots_bot" | grep -q "id"; then
  echo "SKIP: pkg_playerbots_bot table absent — playerbots package not installed/published"
  exit 77
fi

PAD_X=-8930.0; PAD_Y=-250.0; PAD_Z=80.0
REALM_DB="${REALM_DB:-}"
# Authority reads, when there is a separate authority. Empty/equal REALM_DB → the shard IS the
# authority (the single-database plane) and the arm is skipped rather than double-asserted.
rsql1() { spacetime sql "$REALM_DB" "$1" 2>/dev/null | sed -n 3p | awk -F'|' '{gsub(/ /,"",$1); print $1}'; }

# ---- staging: one dps bot + Ginger on the pad, no stale party anywhere ----
scall playerbots_despawn_all || true
leave_any_group "$GINGER" || { echo "[orch] could not leave Ginger's prior party" >&2; exit 1; }
sqlq "DELETE FROM game_group_invite WHERE target_guid = $GINGER" >/dev/null
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z WHERE guid = $GINGER" >/dev/null
scall playerbots_spawn_role 1 $PAD_X $PAD_Y $PAD_Z 2 || { echo "[orch] bot spawn failed" >&2; exit 1; }
BOT=$(char_guid Dpsbot1)
[ -z "$BOT" ] && { echo "[orch] no bot guid" >&2; exit 1; }
echo "[orch] bot-invite: bot=$BOT ginger=$GINGER realm_db=${REALM_DB:-<none>}"
# The state the bug is about: the bot's entity is LIVE while its character row is forever offline
# (it never runs `player_login`), which is exactly how the gateway recognises "nobody is at the
# keyboard here" (`party::session_less_in_world`).
assert_eq "staging: the bot has a live entity" "$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE guid = $BOT")" "1"
assert_eq "staging: …and no session (game_character.online false)" "$(sql1 "SELECT COUNT(*) AS n FROM game_character WHERE guid = $BOT AND online = true")" "0"

# ---- the invite: one wire session, one bot, bounded wait ----
# `party-bots` sends CMSG_GROUP_INVITE and requires SMSG_PARTY_COMMAND_RESULT(Success) AND an
# SMSG_GROUP_LIST carrying the bot within 15s, then writes <hold>.ingroup. Every wait below has a
# deadline: a hang must report as a failure, never as a pass.
HOLD=/tmp/ws_bot_invite_$$
rm -f "$HOLD" "$HOLD.ingroup"
timeout 180 "$WC" TEST Ginger party-bots "$HOLD" Dpsbot1 >/tmp/ws_bot_invite.log 2>&1 &
LEADER=$!
if wait_for_file 60 "$HOLD.ingroup"; then
  step_ok "wire: the player's client decoded SMSG_GROUP_LIST carrying the bot (invite answered)"
else
  step_fail "wire: the bot never answered the invite within 60s — the #51 symptom exactly"
  tail -5 /tmp/ws_bot_invite.log
fi

# ---- membership, on the shard the bot stands on (its own in-world reads use these rows) ----
GROUP_ID=$(sql1 "SELECT group_id FROM game_group_member WHERE character_guid = $GINGER")
[ -n "$GROUP_ID" ] || { echo "[orch] Ginger has no group id after the bot joined" >&2; exit 1; }
assert_eq "sql: fixture group exists on the shard" "$(sql1 "SELECT COUNT(*) AS n FROM game_group WHERE group_id = $GROUP_ID")" "1"
assert_eq "sql: fixture group has two members on the shard" "$(sql1 "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID")" "2"
assert_eq "sql: the PLAYER leads the fixture group" "$(sql1 "SELECT leader_guid FROM game_group WHERE group_id = $GROUP_ID")" "$GINGER"
assert_eq "sql: the bot is a member" "$(sql1 "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $BOT")" "1"
assert_eq "sql: the invite was CONSUMED (a leftover row is the hung dialog)" "$(sql1 "SELECT COUNT(*) AS n FROM game_group_invite WHERE target_guid = $BOT")" "0"

# ---- and on the AUTHORITY, when there is a separate one (#22) — the arm that pins #51 ----
if [ -n "$REALM_DB" ] && [ "$REALM_DB" != "$DB" ]; then
  assert_eq "realm-core: fixture group has two members" "$(rsql1 "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID")" "2"
  assert_eq "realm-core: the bot is a member of the authority's party" "$(rsql1 "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $BOT")" "1"
  assert_eq "realm-core: the pending invite was consumed" "$(rsql1 "SELECT COUNT(*) AS n FROM game_group_invite WHERE target_guid = $BOT")" "0"
else
  echo "[orch] bot-invite: NOTE — no separate REALM_DB, so the party authority IS this database."
  echo "[orch] bot-invite: this run cannot exhibit #51 at all (the module's own hook answers here);"
  echo "[orch] bot-invite: re-run with REALM_DB=<realm-core db> against a LYRACORE_REALM_CORE gateway."
fi

# ---- teardown: release the wire leader (it disbands), then despawn ----
touch "$HOLD"
wait "$LEADER" 2>/dev/null
scall playerbots_despawn_all || true
assert_eq "teardown: fixture group removed from the shard" "$(sql1 "SELECT COUNT(*) AS n FROM game_group WHERE group_id = $GROUP_ID")" "0"
if [ -n "$REALM_DB" ] && [ "$REALM_DB" != "$DB" ]; then
  assert_eq "teardown: fixture group removed from realm-core" "$(rsql1 "SELECT COUNT(*) AS n FROM game_group WHERE group_id = $GROUP_ID")" "0"
fi
assert_eq "teardown: zero bot rows" "$(sql1 "SELECT COUNT(*) AS n FROM pkg_playerbots_bot")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[bot-invite] PASS"; exit 0; else echo "[bot-invite] FAIL"; exit 1; fi
