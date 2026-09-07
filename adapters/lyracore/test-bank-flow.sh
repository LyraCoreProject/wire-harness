#!/usr/bin/env bash
# Operator-run bank acceptance: open -> deposit -> relog -> withdraw -> buy one bank bag slot.
#
# Usage: printf '%s\n' "$PASSWORD" | \
#   LYRACORE_DIR=/path/to/LyraCore bash adapters/lyracore/test-bank-flow.sh \
#   <account> <disposable_character> <banker_entry>
#
# This deletes and recreates the named Character as a level-5 Warrior, moves its starter sword,
# spends 1,000 copper on the first bank bag slot, and spawns one creature from the supplied banker
# template. Run it only against a disposable test Realm. Issue #137 assigns live execution to the
# Operator; repository checks must not run this script against a Realm.
set -uo pipefail

usage() {
  echo "usage: test-bank-flow.sh <account> <disposable_character> <banker_entry> (password on stdin)" >&2
  exit 2
}

[ "$#" -eq 3 ] || usage
ACCOUNT=$1
CHARACTER=$2
BANKER_ENTRY=$3
case "$ACCOUNT" in (*[!A-Za-z0-9_]*|'') usage ;; esac
case "$CHARACTER" in (*[!A-Za-z0-9]*|'') usage ;; esac
case "$BANKER_ENTRY" in (*[!0-9]*|'') usage ;; esac
[ "${#ACCOUNT}" -le 32 ] || usage
[ "${#CHARACTER}" -ge 2 ] && [ "${#CHARACTER}" -le 12 ] || usage
[ "$BANKER_ENTRY" -gt 0 ] 2>/dev/null || usage
case "${DB:-lyracore}" in (-*|*[!A-Za-z0-9_-]*|'') echo "[bank] invalid DB name" >&2; exit 2 ;; esac
IFS= read -r PASSWORD || { echo "[bank] password is required on stdin" >&2; exit 2; }
[ -n "$PASSWORD" ] || { echo "[bank] password is required on stdin" >&2; exit 2; }

source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh"
source "$ADAPTER_DIR/scenario-lib.sh"

# wire.sh reads the account-specific value and passes it to vanilla-wire on stdin. The secret is
# never an argument and neither script prints it.
PASSWORD_VAR="WIRE_PASSWORD_$ACCOUNT"
export "$PASSWORD_VAR=$PASSWORD"
unset PASSWORD
export WIRE_CLASS=warrior

RUN_DIR=$(mktemp -d "${TMPDIR:-/tmp}/wire-bank.XXXXXX") || exit 1
DEPOSITED="$RUN_DIR/deposited"
PERSISTED="$RUN_DIR/persisted"
WITHDRAWN="$RUN_DIR/withdrawn"
WIRE_PID=""
CHAR_GUID=""
BANKER_GUID=""
cleanup() {
  [ -n "$WIRE_PID" ] && kill "$WIRE_PID" 2>/dev/null || true
  [ -n "$WIRE_PID" ] && wait "$WIRE_PID" 2>/dev/null || true
  [ -n "${SC_STAY_PID:-}" ] && stay_stop || true
  [ -n "$BANKER_GUID" ] && sqlq "DELETE FROM game_creature_spawn WHERE guid = $BANKER_GUID" >/dev/null || true
  [ -n "$BANKER_GUID" ] && sqlq "DELETE FROM game_world_entity WHERE guid = $BANKER_GUID" >/dev/null || true
  [ -n "$CHAR_GUID" ] && timeout 60 "$WC" "$ACCOUNT" "$CHARACTER" char-delete >/dev/null 2>&1 || true
  rm -rf "$RUN_DIR"
}
trap cleanup EXIT

wire_build || exit 1
NPC_FLAGS=$(sql1 "SELECT npc_flags FROM game_creature_template WHERE entry = $BANKER_ENTRY")
[ -n "$NPC_FLAGS" ] || { echo "[bank] no creature template $BANKER_ENTRY" >&2; exit 1; }
case "$NPC_FLAGS" in (*[!0-9]*) echo "[bank] invalid npc_flags for template $BANKER_ENTRY" >&2; exit 1 ;; esac
if [ $(( NPC_FLAGS & 256 )) -eq 0 ]; then
  echo "[bank] creature template $BANKER_ENTRY lacks UNIT_NPC_FLAG_BANKER (0x100)" >&2
  exit 1
fi

# Build a clean, known baseline while no world session can overwrite it on disconnect.
timeout 60 "$WC" "$ACCOUNT" "$CHARACTER" char-delete >/dev/null 2>&1 \
  || { echo "[bank] could not clear disposable Character $CHARACTER" >&2; exit 1; }
timeout 60 "$WC" "$ACCOUNT" "$CHARACTER" make-char warrior >/dev/null \
  || { echo "[bank] could not create disposable Character $CHARACTER" >&2; exit 1; }
CHAR_GUID=$(char_guid "$CHARACTER")
[ -n "$CHAR_GUID" ] || { echo "[bank] failed to create $CHARACTER" >&2; exit 1; }
scall debug_set_level "$CHAR_GUID" 5 \
  || { echo "[bank] debug_set_level failed" >&2; exit 1; }
scall debug_set_money "$CHAR_GUID" 1000 \
  || { echo "[bank] debug_set_money failed" >&2; exit 1; }
ITEM_GUID=$(sql1 "SELECT guid FROM game_item_instance WHERE owner_guid = $CHAR_GUID AND slot = 15")
[ -n "$ITEM_GUID" ] || { echo "[bank] fresh Warrior has no starter item in slot 15" >&2; exit 1; }
scall gw_move_item "$CHAR_GUID" 15 23 \
  || { echo "[bank] gw_move_item failed" >&2; exit 1; }

stay_start "$ACCOUNT" "$CHARACTER" || exit 1
BANKER_GUID=$(spawn_at "$CHAR_GUID" "$BANKER_ENTRY" 3)
stay_stop
[ -n "$BANKER_GUID" ] || { echo "[bank] failed to spawn banker $BANKER_ENTRY" >&2; exit 1; }

timeout 180 "$WC" "$ACCOUNT" "$CHARACTER" scenario-bank \
  "$BANKER_GUID" "$ITEM_GUID" 23 39 "$DEPOSITED" "$PERSISTED" "$WITHDRAWN" &
WIRE_PID=$!

wait_for_file 30 "$DEPOSITED" || { echo "[bank] deposit phase did not reach its check" >&2; exit 1; }
assert_eq "deposit preserves item identity in bank slot 39" \
  "$(sql1 "SELECT guid FROM game_item_instance WHERE owner_guid = $CHAR_GUID AND slot = 39")" \
  "$ITEM_GUID"
rm -f "$DEPOSITED"

wait_for_file 45 "$PERSISTED" || { echo "[bank] relog phase did not reach its check" >&2; exit 1; }
assert_eq "relog preserves item identity in bank slot 39" \
  "$(sql1 "SELECT guid FROM game_item_instance WHERE owner_guid = $CHAR_GUID AND slot = 39")" \
  "$ITEM_GUID"
rm -f "$PERSISTED"

wait_for_file 30 "$WITHDRAWN" || { echo "[bank] withdrawal phase did not reach its check" >&2; exit 1; }
assert_eq "withdraw preserves item identity in carry slot 23" \
  "$(sql1 "SELECT guid FROM game_item_instance WHERE owner_guid = $CHAR_GUID AND slot = 23")" \
  "$ITEM_GUID"
rm -f "$WITHDRAWN"

WIRE_RC=0
wait "$WIRE_PID" || WIRE_RC=$?
WIRE_PID=""
[ "$WIRE_RC" -eq 0 ] || { echo "[bank] wire scenario failed (rc=$WIRE_RC)" >&2; exit 1; }

# Disconnect persistence is asynchronous. Both fields must land together: the first rung costs
# exactly 1,000 copper and changes the durable owned-slot count from zero to one.
DURABLE=""
for _ in $(seq 1 15); do
  DURABLE=$(sql1 "SELECT bank_bag_slots FROM game_character WHERE guid = $CHAR_GUID")
  MONEY=$(sql1 "SELECT money FROM game_character WHERE guid = $CHAR_GUID")
  [ "$DURABLE" = "1" ] && [ "$MONEY" = "0" ] && break
  sleep 1
done
assert_eq "bank bag purchase persists one owned slot" "$DURABLE" "1"
assert_eq "first bank bag slot costs 1,000 copper" "$MONEY" "0"

if [ "$FAILED" -eq 0 ]; then
  echo "[bank] PASS: protocol flow and durable state checks completed"
  exit 0
fi
echo "[bank] FAIL" >&2
exit 1
