#!/usr/bin/env bash
# RELAY DELIVERY UNDER FAT TRANSACTIONS (279) — the 277 loss-class regression net:
#   `debug_stress_relay` packs ~250 subscription-irrelevant rows + a quest kill credit + an item
#   grant into ONE transaction (the shape that silently swallowed the teleport event pre-277).
#   A live wire session must still receive SMSG_QUESTUPDATE_ADD_KILL (0x199) and
#   SMSG_ITEM_PUSH_RESULT (0x166) out of that transaction — proving the coordinator-registered
#   relays deliver regardless of transaction size or per-player AOI subscription churn.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
source "$ADAPTER_DIR/scenario-lib.sh"
scenario_preflight relay-stress

WOLF=51000; GIVER=51003; QUEST=50900; WATER=159
PAD_X=-8930.0; PAD_Y=-250.0; PAD_Z=80.0

stress_watch() { # $1=opcode-decimal  $2=label
  local OUT; OUT=$(mktemp)
  timeout 45 "$WC" TEST Ginger opcode-watch "$1" 25 >"$OUT" 2>&1 &
  local WPID=$!
  sleep 6   # login + watch armed
  # (Re)stage inside the session: giver near Ginger, quest in the log (idempotent re-accept of a
  # rewarded repeatable resets it; an active row makes accept fail harmlessly — credit still lands).
  scall debug_spawn_at_feet "$GINGER" $GIVER 3
  G=$(sqlq "SELECT guid FROM game_world_entity WHERE entry = $GIVER" | grep -oE '[0-9]{15,}' | head -1)
  scall debug_accept_quest "$GINGER" "$G" $QUEST
  scall debug_stress_relay "$GINGER" $WOLF $WATER 250
  wait $WPID 2>/dev/null
  if grep -q 'OPCODE-WATCH PASS' "$OUT"; then
    step_ok "$2 delivered out of the fat transaction"
  else
    step_fail "$2 NOT delivered (fat-transaction relay loss)"; tail -2 "$OUT"
  fi
  rm -f "$OUT"
  purge_entry_rows $GIVER
}

# ---- staging ----
scall playerbots_despawn_all || true
purge_entry_rows $GIVER
sqlq "DELETE FROM game_character_quest WHERE character_guid = $GINGER" >/dev/null
sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z, map_id = 0 WHERE guid = $GINGER" >/dev/null

# ---- the two relay families, each twice (the done-when's "twice consecutively") ----
stress_watch 409 "quest relay (SMSG_QUESTUPDATE_ADD_KILL): run 1"
stress_watch 409 "quest relay (SMSG_QUESTUPDATE_ADD_KILL): run 2"
stress_watch 358 "item relay (SMSG_ITEM_PUSH_RESULT): run 1"
stress_watch 358 "item relay (SMSG_ITEM_PUSH_RESULT): run 2"

# ---- teardown ----
sqlq "DELETE FROM game_character_quest WHERE character_guid = $GINGER" >/dev/null
purge_entry_rows $GIVER
if [ "$FAILED" -eq 0 ]; then echo "[relay-stress] PASS"; exit 0; else echo "[relay-stress] FAIL"; exit 1; fi
