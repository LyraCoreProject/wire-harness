#!/usr/bin/env bash
# Character deletion must remove only that Character from the Realm-core party and every Shard
# mirror. A three-member fixture keeps the party alive, which lets this scenario inspect the two
# surviving members instead of accepting a blanket disband. The same named lifecycle runs twice.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh"
source "$ADAPTER_DIR/scenario-lib.sh"
scenario_preflight party-delete
ensure_playerbots_package party-delete

RC=$(party_authority_db)
NAME=Wspartydel
PAD_X=-8930.0; PAD_Y=-250.0; PAD_Z=80.0

read_one() { sql1_required "$2" "$1"; } # $1=database $2=query

wait_value() { # $1=seconds $2=database $3=query $4=want
  local seconds=$1 database=$2 query=$3 want=$4 value=""
  for _ in $(seq 1 "$seconds"); do
    value=$(read_one "$database" "$query") || return 1
    [ "$value" = "$want" ] && return 0
    sleep 1
  done
  echo "[party-delete] timed out on '$database': got '$value', want '$want' for $query" >&2
  return 1
}

bot_guids() {
  local output
  if ! output=$(spacetime sql "$DB" "SELECT character_guid FROM pkg_playerbots_bot" 2>&1); then
    echo "[party-delete] could not read the bot roster on '$DB'" >&2
    echo "$output" >&2
    return 1
  fi
  printf '%s\n' "$output" | sed -n '3,$p' | grep -oE '[0-9]+' || true
}

for PASS in 1 2; do
  echo "[party-delete] lifecycle $PASS"
  timeout 60 "$WC" TEST "$NAME" char-delete >/dev/null 2>&1 \
    || { echo "[party-delete] could not clear the prior $NAME fixture" >&2; exit 1; }
  timeout 60 "$WC" TEST "$NAME" make-char warrior >/dev/null 2>&1 \
    || { echo "[party-delete] could not create $NAME" >&2; exit 1; }
  DELETED=$(char_guid "$NAME")
  [ -n "$DELETED" ] || { echo "[party-delete] no guid for $NAME" >&2; exit 1; }

  BEFORE=$(bot_guids) || exit 1
  BEFORE=$(printf '%s\n' "$BEFORE" | tr '\n' ' ')
  scall playerbots_spawn_role 2 $PAD_X $PAD_Y $PAD_Z 2 \
    || { echo "[party-delete] bot spawn failed" >&2; exit 1; }
  CREATED=""
  AFTER=$(bot_guids) || exit 1
  for BOT in $AFTER; do
    case " $BEFORE " in *" $BOT "*) ;; *) CREATED="$CREATED $BOT" ;; esac
  done
  read -r BOT_A BOT_B EXTRA <<<"${CREATED# }"
  { [ -z "${BOT_A:-}" ] || [ -z "${BOT_B:-}" ] || [ -n "${EXTRA:-}" ]; } \
    && { echo "[party-delete] expected exactly two new bot guids, got '$CREATED'" >&2; exit 1; }
  BOT_A_NAME=$(read_one "$DB" "SELECT name FROM game_character WHERE guid = $BOT_A") || exit 1
  BOT_B_NAME=$(read_one "$DB" "SELECT name FROM game_character WHERE guid = $BOT_B") || exit 1

  HOLD=/tmp/ws_party_delete_$$_$PASS
  rm -f "$HOLD" "$HOLD.ingroup"
  timeout 240 "$WC" TEST "$NAME" party-bots "$HOLD" "$BOT_A_NAME" "$BOT_B_NAME" \
    >"/tmp/ws_party_delete_$PASS.log" 2>&1 &
  LEADER=$!
  wait_for_file 60 "$HOLD.ingroup"
  [ -f "$HOLD.ingroup" ] \
    || { echo "[party-delete] fixture party did not form" >&2; tail -5 "/tmp/ws_party_delete_$PASS.log"; exit 1; }

  wait_value 30 "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $DELETED" 1 \
    || exit 1
  GROUP_ID=$(read_one "$DB" "SELECT group_id FROM game_group_member WHERE character_guid = $DELETED") \
    || exit 1
  wait_value 30 "$RC" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID" 3 \
    || exit 1
  assert_eq "lifecycle $PASS: Shard fixture has three members" \
    "$(read_one "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID")" "3"

  # End the World Session without sending CMSG_GROUP_DISBAND, then delete at Character Select.
  kill "$LEADER" 2>/dev/null || true
  wait "$LEADER" 2>/dev/null || true
  wait_value 30 "$DB" "SELECT COUNT(*) AS n FROM game_character WHERE guid = $DELETED AND online = false" 1 \
    || exit 1
  timeout 60 "$WC" TEST "$NAME" char-delete >"/tmp/ws_party_delete_char_$PASS.log" 2>&1 \
    || { tail -5 "/tmp/ws_party_delete_char_$PASS.log" >&2; exit 1; }

  wait_value 30 "$RC" "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $DELETED" 0 \
    || exit 1
  wait_value 30 "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $DELETED" 0 \
    || exit 1
  wait_value 30 "$RC" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID" 2 \
    || exit 1
  wait_value 30 "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID" 2 \
    || exit 1
  assert_eq "lifecycle $PASS: Realm-core keeps both survivors" \
    "$(read_one "$RC" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID AND (character_guid = $BOT_A OR character_guid = $BOT_B)")" "2"
  assert_eq "lifecycle $PASS: Shard mirror keeps both survivors" \
    "$(read_one "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID AND (character_guid = $BOT_A OR character_guid = $BOT_B)")" "2"
  assert_eq "lifecycle $PASS: deleted Character row is gone" \
    "$(read_one "$DB" "SELECT COUNT(*) AS n FROM game_character WHERE guid = $DELETED")" "0"

  scall debug_delete_character "$BOT_A" \
    || { echo "[party-delete] bot $BOT_A teardown failed" >&2; exit 1; }
  scall debug_delete_character "$BOT_B" \
    || { echo "[party-delete] bot $BOT_B teardown failed" >&2; exit 1; }
  wait_value 30 "$RC" "SELECT COUNT(*) AS n FROM game_group WHERE group_id = $GROUP_ID" 0 || exit 1
  wait_value 30 "$DB" "SELECT COUNT(*) AS n FROM game_group WHERE group_id = $GROUP_ID" 0 || exit 1
  assert_eq "lifecycle $PASS: no Realm-core fixture members remain" \
    "$(read_one "$RC" "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $DELETED OR character_guid = $BOT_A OR character_guid = $BOT_B")" "0"
  assert_eq "lifecycle $PASS: no Shard fixture members remain" \
    "$(read_one "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $DELETED OR character_guid = $BOT_A OR character_guid = $BOT_B")" "0"
  rm -f "$HOLD" "$HOLD.ingroup"
done

if [ "$FAILED" -eq 0 ]; then echo "[party-delete] PASS"; exit 0; else echo "[party-delete] FAIL"; exit 1; fi
