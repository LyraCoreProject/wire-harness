#!/usr/bin/env bash
# SCENARIO 2 — VENDOR LOOP (work-item 140): buy -> equip (stat fold via compute-swing readout) ->
# fight until durability drops -> repair (gold accounting) -> sell -> buyback. Wire actions are the
# vendor micro-modes (vendor-list/vendor-buy/equip-from/unequip-from/vendor-sell/vendor-repair/
# vendor-buyback), each its own session; this orchestrator sequences them and sql-asserts the
# server state between every step. Fixture: vendor 51004 sells Tempered Blade (50, 1200c, 70 dura).
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh
scenario_preflight scenario-vendor

VENDOR_ENTRY=51004; BAG_ENTRY=51001 # the 0-damage friendly trainer template doubles as a punching bag
BLADE=50
PAD_X=-8900; PAD_Y=-210; PAD_Z=82
# Run-scoped handshake paths (work-item 161): defined ONCE here, passed as wire-client args.
SOLD_FILE=/tmp/ws_vendor_sold_$$
BOUGHT_FILE=/tmp/ws_vendor_bought_$$

# repeatability: purge any stale blade instances + buyback rows from a prior run
sqlq "DELETE FROM game_item_instance WHERE owner_guid = $GINGER AND entry = $BLADE" >/dev/null
sqlq "DELETE FROM game_character_buyback WHERE player_guid = $GINGER" >/dev/null
scall debug_grant_money "$GINGER" 2000

stay_start TEST test123 Ginger || exit 1
scall debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0
scall debug_set_health "$GINGER" 100000
VENDOR=$(spawn_at "$GINGER" $VENDOR_ENTRY 4)
BAG=$(spawn_at "$GINGER" $BAG_ENTRY 6)
if [ -z "$VENDOR" ] || [ -z "$BAG" ]; then echo "[orch] fixture spawn failed" >&2; stay_stop; exit 1; fi
# baseline swing readout (pre-equip): unarmed/starter-weapon damage
scall debug_compute_swing "$GINGER" "$BAG"
SWING0=$(sql1 "SELECT final_max FROM game_debug_readout WHERE key = 'swing'")
stay_stop
echo "[orch] staged: vendor=$VENDOR bag=$BAG swing0=${SWING0:-?}"

# STEP 1: the vendor window lists the blade
timeout 60 "$WC" TEST test123 Ginger vendor-list "$VENDOR" $BLADE || FAILED=1

# STEP 2: buy — money falls by buy_price, a fresh instance appears in the backpack
timeout 60 "$WC" TEST test123 Ginger vendor-buy "$VENDOR" $BLADE || FAILED=1
sleep 1
assert_eq "buy: money 2000 -> 800 (buy_price 1200)" "$(sql1 "SELECT money FROM game_character WHERE guid = $GINGER")" "800"
ROW=$(sqlq "SELECT guid, slot, durability FROM game_item_instance WHERE owner_guid = $GINGER AND entry = $BLADE" | sed -n 3p)
IGUID=$(awk -F'|' '{gsub(/ /,"",$1); print $1}' <<<"$ROW"); ISLOT=$(awk -F'|' '{gsub(/ /,"",$2); print $2}' <<<"$ROW")
assert_ge "buy: blade instance in a backpack slot (>=23)" "${ISLOT:-0}" 23
assert_eq "buy: full durability" "$(awk -F'|' '{gsub(/ /,"",$3); print $3}' <<<"$ROW")" "70"

# STEP 3: equip over the wire — slot moves to main-hand (15), swing readout max rises (stat fold)
timeout 60 "$WC" TEST test123 Ginger equip-from "${ISLOT:-0}" || FAILED=1
sleep 1
assert_eq "equip: blade now in main-hand slot 15" "$(sql1 "SELECT slot FROM game_item_instance WHERE guid = ${IGUID:-0}")" "15"
stay_start TEST test123 Ginger || exit 1
scall debug_compute_swing "$GINGER" "$BAG"
SWING1=$(sql1 "SELECT final_max FROM game_debug_readout WHERE key = 'swing'")
assert_gt "equip: compute-swing max rose with the blade (stat fold)" "${SWING1:-0}" "${SWING0:-0}"

# STEP 4: fight until durability drops (real swings vs the 0-damage bag; 10%/swing wear)
scall debug_engage "$GINGER" "$BAG"
DUR=""
for _ in $(seq 1 60); do
  DUR=$(sql1 "SELECT durability FROM game_item_instance WHERE guid = ${IGUID:-0}")
  [ -n "$DUR" ] && [ "$DUR" -lt 70 ] && break
  sleep 2
done
sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = ${BAG:-0}" >/dev/null
stay_stop
assert_lt "fight: durability wore below 70" "${DUR:-70}" 70

# STEP 5: repair — durability back to max, money falls by the repair cost
MONEY_PRE_REPAIR=$(sql1 "SELECT money FROM game_character WHERE guid = $GINGER")
timeout 60 "$WC" TEST test123 Ginger vendor-repair "$VENDOR" "${IGUID:-0}" || FAILED=1
sleep 1
assert_eq "repair: durability restored to 70" "$(sql1 "SELECT durability FROM game_item_instance WHERE guid = ${IGUID:-0}")" "70"
assert_lt "repair: money fell by the repair cost" "$(sql1 "SELECT money FROM game_character WHERE guid = $GINGER")" "${MONEY_PRE_REPAIR:-0}"

# STEP 6: unequip (back to backpack), then sell — money +sell_price, instance leaves the inventory
timeout 60 "$WC" TEST test123 Ginger unequip-from 15 || FAILED=1
sleep 1
NSLOT=$(sql1 "SELECT slot FROM game_item_instance WHERE guid = ${IGUID:-0}")
assert_ge "unequip: blade back in a backpack slot" "${NSLOT:-0}" 23
# STEP 6b+7: sell then buyback in ONE session (logout clears the buyback ring — vanilla), with
# mid-session sql assertions at the two handshake points.
MONEY_PRE_SELL=$(sql1 "SELECT money FROM game_character WHERE guid = $GINGER")
rm -f "$SOLD_FILE" "$BOUGHT_FILE"
timeout 120 "$WC" TEST test123 Ginger vendor-sell-buyback "$VENDOR" "${IGUID:-0}" "$SOLD_FILE" "$BOUGHT_FILE" &
WIRE=$!
wait_for_file 20 "$SOLD_FILE"
if [ -f "$SOLD_FILE" ]; then
  sleep 1
  # mid-session, the authoritative purse is the LIVE entity (game_character.money persists at logout)
  assert_eq "sell: money +240 (sell_price)" "$(sql1 "SELECT money FROM game_world_entity WHERE guid = $GINGER")" "$(( ${MONEY_PRE_SELL:-0} + 240 ))"
  assert_eq "sell: instance out of the inventory" "$(sql1 "SELECT COUNT(*) AS n FROM game_item_instance WHERE guid = ${IGUID:-0}")" "0"
  assert_ge "sell: buyback ring row exists" "$(sql1 "SELECT COUNT(*) AS n FROM game_character_buyback WHERE player_guid = $GINGER")" 1
  MONEY_PRE_BB=$(sql1 "SELECT money FROM game_world_entity WHERE guid = $GINGER")
  rm -f "$SOLD_FILE"
else
  echo "[orch] wire client never signalled the sell" >&2; FAILED=1
fi
wait_for_file 20 "$BOUGHT_FILE"
if [ -f "$BOUGHT_FILE" ]; then
  sleep 1
  assert_eq "buyback: money -240" "$(sql1 "SELECT money FROM game_world_entity WHERE guid = $GINGER")" "$(( ${MONEY_PRE_BB:-0} - 240 ))"
  assert_ge "buyback: blade instance back in the inventory" "$(sql1 "SELECT COUNT(*) AS n FROM game_item_instance WHERE owner_guid = $GINGER AND entry = $BLADE")" 1
  rm -f "$BOUGHT_FILE"
else
  echo "[orch] wire client never signalled the buyback" >&2; FAILED=1
fi
wait "$WIRE"; RC=$?
[ $RC -ne 0 ] && { echo "[orch] sell/buyback wire mode failed (rc=$RC)"; FAILED=1; }

# ---- teardown (asserted) ----
# The unequip step left main-hand slot 15 empty — restore a starter sword so gear-shaped tests
# (char-enum-gear, the death scenario's durability claim) keep their fixture invariant.
if [ -z "$(sql1 "SELECT durability FROM game_item_instance WHERE owner_guid = $GINGER AND slot = 15")" ]; then
  stay_start TEST test123 Ginger && scall debug_grant_item "$GINGER" 25 1
  stay_stop
  SWORD_SLOT=$(sqlq "SELECT slot FROM game_item_instance WHERE owner_guid = $GINGER AND entry = 25" | grep -oE '[0-9]+' | tail -1)
  [ -n "$SWORD_SLOT" ] && timeout 60 "$WC" TEST test123 Ginger equip-from "$SWORD_SLOT" >/dev/null 2>&1
  assert_ge "teardown: main-hand restored (slot 15 occupied)" "$(sql1 "SELECT COUNT(*) AS n FROM game_item_instance WHERE owner_guid = $GINGER AND slot = 15")" 1
fi
sqlq "DELETE FROM game_item_instance WHERE owner_guid = $GINGER AND entry = $BLADE" >/dev/null
sqlq "DELETE FROM game_character_buyback WHERE player_guid = $GINGER" >/dev/null
scall debug_grant_money "$GINGER" 0
purge_entry $VENDOR_ENTRY
sqlq "DELETE FROM game_creature_spawn WHERE entry = $BAG_ENTRY AND x > -8910" >/dev/null
sqlq "DELETE FROM game_world_entity WHERE guid = ${BAG:-0}" >/dev/null
assert_eq "teardown: no blade instances remain" "$(sql1 "SELECT COUNT(*) AS n FROM game_item_instance WHERE owner_guid = $GINGER AND entry = $BLADE")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[scenario-vendor] PASS"; exit 0; else echo "[scenario-vendor] FAIL"; exit 1; fi
