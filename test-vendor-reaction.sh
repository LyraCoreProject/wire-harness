#!/usr/bin/env bash
# test-vendor-reaction.sh — 195 slice A: standing-derived reaction gates the vendor WINDOW.
# A vendor whose parent faction has a rep bar refuses CMSG_LIST_INVENTORY from a player whose
# standing rank is Unfriendly or below; at Neutral+ the window opens. Drives the REAL wire path
# (vendor-list asserts SMSG_LIST_INVENTORY) both ways around a standing flip.
#
# Fixture: vendor 51004 re-pointed (entity-level, restored after) at faction_template 50901 —
# a script-managed template whose parent is the scenario fixture faction 50900 (rep index 60,
# seeded by debug_seed_scenario_fixtures). Standing moves via debug_grant_reputation.
set -uo pipefail
cd "$(dirname "$0")/../.."
DB=spacetime-core
WC=./target/debug/wire-client
source tools/wire-client/scenario-lib.sh

GINGER=$(char_guid Ginger)
[ -z "$GINGER" ] && { echo "[vendor-reaction] no Ginger" >&2; exit 1; }
VENDOR_ENTRY=51004
BLADE=5090050
PAD_X=-8900; PAD_Y=-210; PAD_Z=82
FAILED=0

# Script-managed fixture faction_template 50901 -> parent 50900 (idempotent).
if [ "$(sql1 "SELECT COUNT(*) AS n FROM game_faction_template WHERE id = 50901")" = "0" ]; then
  sqlq "INSERT INTO game_faction_template (id, faction, faction_group, friend_group, enemy_group, enemy_0, enemy_1, enemy_2, enemy_3, friend_0, friend_1, friend_2, friend_3) VALUES (50901, 50900, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0)" >/dev/null
fi

stay_start TEST test123 Ginger || exit 1
scall debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0
VENDOR=$(spawn_at "$GINGER" $VENDOR_ENTRY 4)
[ -z "$VENDOR" ] && { echo "[vendor-reaction] vendor spawn failed" >&2; stay_stop; exit 1; }
# Point the LIVE vendor entity at the rep-bar template (entity-scoped; the spawn row is deleted in teardown).
sqlq "UPDATE game_world_entity SET faction_template = 50901 WHERE guid = $VENDOR" >/dev/null
stay_stop

# Baseline the player's standing with 50900 at HOSTILE (-6000): current standing may carry residue
# from other runs, so read it and grant the delta to land exactly at -6000.
CUR=$(sql1 "SELECT standing FROM game_player_reputation WHERE character_guid = $GINGER AND faction_id = 50900"); CUR=${CUR:-0}
scall debug_grant_reputation "$GINGER" 50900 "$(( -6000 - CUR ))"

# STEP 1: HOSTILE standing -> the window is REFUSED (vendor-list must FAIL its assert).
if timeout 30 "$WC" TEST test123 Ginger vendor-list "$VENDOR" $BLADE >/dev/null 2>&1; then
  echo "[vendor-reaction] STEP-ASSERT FAIL: hostile-standing vendor still opened the window" >&2
  FAILED=1
else
  echo "[vendor-reaction] STEP-ASSERT OK: hostile standing refuses SMSG_LIST_INVENTORY"
fi

# STEP 2: back to NEUTRAL (0) -> the window opens again (proves the gate keys on standing, not breakage).
scall debug_grant_reputation "$GINGER" 50900 6000
if timeout 60 "$WC" TEST test123 Ginger vendor-list "$VENDOR" $BLADE >/dev/null 2>&1; then
  echo "[vendor-reaction] STEP-ASSERT OK: neutral standing opens the window"
else
  echo "[vendor-reaction] STEP-ASSERT FAIL: neutral standing did not open the window" >&2
  FAILED=1
fi

# Teardown: standing back to the pre-test residue, despawn the vendor (entity + spawn row).
scall debug_grant_reputation "$GINGER" 50900 "$(( CUR - 0 ))" || true
sqlq "DELETE FROM game_creature_spawn WHERE entry = $VENDOR_ENTRY" >/dev/null
sqlq "DELETE FROM game_world_entity WHERE guid = $VENDOR" >/dev/null

if [ "$FAILED" = "0" ]; then echo "[vendor-reaction] PASS"; else echo "[vendor-reaction] FAIL"; exit 1; fi
