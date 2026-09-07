#!/usr/bin/env bash
# Character deletion must remove only that Character from the Realm-core party and every Shard
# mirror. A three-member fixture keeps the party alive so both surviving members can be checked.
# The same named lifecycle runs twice.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh"
source "$ADAPTER_DIR/scenario-lib.sh"
scenario_preflight party-delete

RC=$(party_authority_db) || exit 1
NAME=Wspartydel
SURVIVOR_A_NAME=Wspartyone
SURVIVOR_B_NAME=Wspartytwo
OTHER_A_NAME=Wsotherone
OTHER_B_NAME=Wsothertwo

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

wire_delete() { # $1=Character name $2=log label
  timeout 60 "$WC" TEST "$1" char-delete >"/tmp/ws_party_delete_$2.log" 2>&1 \
    || { tail -5 "/tmp/ws_party_delete_$2.log" >&2; return 1; }
}

wire_create() { # $1=Character name $2=log label
  timeout 60 "$WC" TEST "$1" make-char warrior >"/tmp/ws_party_delete_$2.log" 2>&1 \
    || { tail -5 "/tmp/ws_party_delete_$2.log" >&2; return 1; }
}

party_call() { # $1=op $2=actor guid $3=target guid
  local actor
  actor=$(operator_actor "$2") || return $?
  spacetime call "$RC" -- realm_group_op "$1" "$actor" "$3" 0 0 >/dev/null
}

assert_unrelated_party() { # $1=lifecycle label $2=database $3=plane label
  local label=$1 database=$2 plane=$3
  assert_eq "$label: unrelated $plane party row and settings remain" \
    "$(read_one "$database" "SELECT COUNT(*) AS n FROM game_group WHERE group_id = $OTHER_GROUP AND leader_guid = $OTHER_A AND loot_method = 3 AND loot_threshold = 2 AND master_looter_guid = 0")" "1"
  assert_eq "$label: unrelated $plane party keeps both members" \
    "$(read_one "$database" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $OTHER_GROUP AND (character_guid = $OTHER_A OR character_guid = $OTHER_B)")" "2"
}

for FIXTURE_NAME in "$OTHER_A_NAME" "$OTHER_B_NAME"; do
  [ -z "$(char_guid "$FIXTURE_NAME")" ] \
    || wire_delete "$FIXTURE_NAME" "clear_$FIXTURE_NAME" || exit 1
  wire_create "$FIXTURE_NAME" "create_$FIXTURE_NAME" || exit 1
done
OTHER_A=$(char_guid "$OTHER_A_NAME")
OTHER_B=$(char_guid "$OTHER_B_NAME")
party_call 0 "$OTHER_A" "$OTHER_B" || exit 1
party_call 1 "$OTHER_B" 0 || exit 1
OTHER_GROUP=$(read_one "$RC" "SELECT group_id FROM game_group_member WHERE character_guid = $OTHER_A") \
  || exit 1
sync_operator_group_mirror "$DB" "$OTHER_GROUP" "$OTHER_A" 3 2 0 \
  "[$OTHER_A,$OTHER_B]" "$OTHER_A" >/dev/null || exit 1

for PASS in 1 2; do
  echo "[party-delete] lifecycle $PASS"
  for FIXTURE_NAME in "$NAME" "$SURVIVOR_A_NAME" "$SURVIVOR_B_NAME"; do
    [ -z "$(char_guid "$FIXTURE_NAME")" ] \
      || wire_delete "$FIXTURE_NAME" "clear_${FIXTURE_NAME}_$PASS" || exit 1
    wire_create "$FIXTURE_NAME" "create_${FIXTURE_NAME}_$PASS" || exit 1
  done
  DELETED=$(char_guid "$NAME")
  SURVIVOR_A=$(char_guid "$SURVIVOR_A_NAME")
  SURVIVOR_B=$(char_guid "$SURVIVOR_B_NAME")
  { [ -n "$DELETED" ] && [ -n "$SURVIVOR_A" ] && [ -n "$SURVIVOR_B" ]; } \
    || { echo "[party-delete] fixture Character creation did not produce three guids" >&2; exit 1; }

  party_call 0 "$DELETED" "$SURVIVOR_A" || exit 1
  party_call 1 "$SURVIVOR_A" 0 || exit 1
  party_call 0 "$DELETED" "$SURVIVOR_B" || exit 1
  party_call 1 "$SURVIVOR_B" 0 || exit 1
  GROUP_ID=$(read_one "$RC" "SELECT group_id FROM game_group_member WHERE character_guid = $DELETED") \
    || exit 1
  wait_value 30 "$RC" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID" 3 \
    || exit 1

  # The Operator calls above stage Realm-core directly. Seed the matching Shard mirror, then let
  # the Gateway's Character-deletion Relay own every later party and mirror change.
  sync_operator_group_mirror "$DB" "$GROUP_ID" "$DELETED" 0 2 0 \
    "[$DELETED,$SURVIVOR_A,$SURVIVOR_B]" "$DELETED" >/dev/null || exit 1
  wait_value 30 "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID" 3 \
    || exit 1

  wire_delete "$NAME" "delete_$PASS" || exit 1
  wait_value 30 "$RC" "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $DELETED" 0 \
    || exit 1
  wait_value 30 "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $DELETED" 0 \
    || exit 1
  wait_value 30 "$RC" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID" 2 \
    || exit 1
  wait_value 30 "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID" 2 \
    || exit 1
  assert_eq "lifecycle $PASS: Realm-core keeps both survivors" \
    "$(read_one "$RC" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID AND (character_guid = $SURVIVOR_A OR character_guid = $SURVIVOR_B)")" "2"
  assert_eq "lifecycle $PASS: Shard mirror keeps both survivors" \
    "$(read_one "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE group_id = $GROUP_ID AND (character_guid = $SURVIVOR_A OR character_guid = $SURVIVOR_B)")" "2"
  assert_eq "lifecycle $PASS: deleted Character row is gone" \
    "$(read_one "$DB" "SELECT COUNT(*) AS n FROM game_character WHERE guid = $DELETED")" "0"

  wire_delete "$SURVIVOR_A_NAME" "delete_a_$PASS" || exit 1
  wire_delete "$SURVIVOR_B_NAME" "delete_b_$PASS" || exit 1
  wait_value 30 "$RC" "SELECT COUNT(*) AS n FROM game_group WHERE group_id = $GROUP_ID" 0 || exit 1
  wait_value 30 "$DB" "SELECT COUNT(*) AS n FROM game_group WHERE group_id = $GROUP_ID" 0 || exit 1
  assert_eq "lifecycle $PASS: no Realm-core fixture members remain" \
    "$(read_one "$RC" "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $DELETED OR character_guid = $SURVIVOR_A OR character_guid = $SURVIVOR_B")" "0"
  assert_eq "lifecycle $PASS: no Shard fixture members remain" \
    "$(read_one "$DB" "SELECT COUNT(*) AS n FROM game_group_member WHERE character_guid = $DELETED OR character_guid = $SURVIVOR_A OR character_guid = $SURVIVOR_B")" "0"
  assert_unrelated_party "lifecycle $PASS" "$RC" Realm-core
  assert_unrelated_party "lifecycle $PASS" "$DB" "Shard mirror"
done

wire_delete "$OTHER_A_NAME" delete_other_a || exit 1
wire_delete "$OTHER_B_NAME" delete_other_b || exit 1
wait_value 30 "$RC" "SELECT COUNT(*) AS n FROM game_group WHERE group_id = $OTHER_GROUP" 0 || exit 1
wait_value 30 "$DB" "SELECT COUNT(*) AS n FROM game_group WHERE group_id = $OTHER_GROUP" 0 || exit 1

if [ "$FAILED" -eq 0 ]; then echo "[party-delete] PASS"; exit 0; else echo "[party-delete] FAIL"; exit 1; fi
