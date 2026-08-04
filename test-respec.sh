#!/usr/bin/env bash
# test-respec.sh — headless verification for work-item #198 (talent respec).
#
# Verifies: resetting talents at a trainer deletes the character's learned `game_character_talent`
# rows, strips the PASSIVE aura(s) those talents applied directly (`cancel_aura` refuses a PASSIVE
# spell, so a respec can't reuse it — deleting `game_aura` rows is the correct unwind, not a relog),
# forgets any ability the reset talents had TAUGHT (`game_player_spell`), and charges the escalating
# gold cost while persisting `respec_count` so a second reset prices the NEXT step up.
#
# No wire-client session is needed for the reducer calls themselves (`reset_talents` isn't gateway-
# wired yet — gossip surfacing is a follow-up, work-item 198's note); this is a pure scall/sqlq probe
# (the `test-persist-health.sh`/`t_bindpoint` shape), just parked in its own file because it has more
# steps (two resets + four assertions each) than a `t_*` one-liner in wire-suite.sh comfortably holds.
# A `stay` session is still opened to give Ginger a LIVE `game_world_entity` — `do_learn_talent` and
# `do_reset_talents` both require one.
#
# Fixture: the seeded Profession Trainer (51001, npc_flags GOSSIP|TRAINER — same fixture
# test-scenario-train.sh trains from) spawned at Ginger's feet. Cruelty (talent 1 — PASSIVE aura
# 51000) + Death Wish (talent 3 — TEACHES active 51300) give one aura-bearing and one
# spellbook-granting talent to reset in the same probe.
#
# `respec_count` only ever increases (deliberately no reset debug reducer — it is a durable
# escalation counter, not test scaffolding), so a repeat run of this script must not assume it starts
# at 0. It reads the count back BEFORE each reset and computes the expected charge from the SAME
# step table `talent::respec_cost_copper` implements (mirrored here in shell), instead of hardcoding
# 10000/50000 — the `t_init_factions` "reputation accumulates across runs" precedent in wire-suite.sh.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh

TRAINER_ENTRY=51001
CRUELTY_TALENT=1; CRUELTY_AURA=51000
DEATHWISH_TALENT=3; DEATHWISH_SPELL=51300
PAD_X=-8880; PAD_Y=-260; PAD_Z=82

GINGER=$(char_guid Ginger)
[ -z "$GINGER" ] && { echo "[198] ERROR: Ginger not found" >&2; exit 1; }

# Mirrors talent::respec_cost_copper exactly (unit-tested in Rust as
# talent::tests::respec_cost_escalates_then_caps) — the shell probe predicts the charge from the
# SAME step table rather than hardcoding it, so a rerun with an already-elevated respec_count still
# asserts a precise amount.
respec_cost() { # $1 = respec_count BEFORE this reset
  local n=$1 cost
  if [ "$n" -eq 0 ]; then cost=10000
  elif [ "$n" -eq 1 ]; then cost=50000
  elif [ "$n" -eq 2 ]; then cost=100000
  else cost=$(( 100000 + 50000 * (n - 2) )); fi
  [ "$cost" -gt 500000 ] && cost=500000
  echo "$cost"
}

scall debug_seed_talents
scall debug_set_money "$GINGER" 2000000 # comfortably above the 500000 cap, however high respec_count has climbed
# repeatability: forget Cruelty/Death Wish + drop their residue before relearning (the
# test-scenario-train.sh "forget the spell + normalize the purse before buying it again" idiom)
sqlq "DELETE FROM game_character_talent WHERE character_guid = $GINGER" >/dev/null
sqlq "DELETE FROM game_aura WHERE target_guid = $GINGER AND spell_id = $CRUELTY_AURA" >/dev/null
sqlq "DELETE FROM game_player_spell WHERE character_guid = $GINGER AND spell_id = $DEATHWISH_SPELL" >/dev/null

stay_start TEST Ginger || exit 1
scall debug_teleport "$GINGER" 0 $PAD_X $PAD_Y $PAD_Z 0
scall debug_set_level "$GINGER" 20 # >= 2 talent points free for Cruelty + Death Wish
TRAINER=$(spawn_at "$GINGER" $TRAINER_ENTRY 4)
if [ -z "$TRAINER" ]; then echo "[198] trainer spawn failed" >&2; stay_stop; exit 1; fi
echo "[orch] staged: trainer=$TRAINER"

scall debug_learn_talent "$GINGER" $CRUELTY_TALENT    # rank 1 -> applies aura 51000
scall debug_learn_talent "$GINGER" $DEATHWISH_TALENT  # rank 1 -> teaches spell 51300

assert_eq "setup: 2 talents learned" "$(sql1 "SELECT COUNT(*) AS n FROM game_character_talent WHERE character_guid = $GINGER")" "2"
assert_eq "setup: Cruelty aura present" "$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE target_guid = $GINGER AND spell_id = $CRUELTY_AURA")" "1"
assert_eq "setup: Death Wish taught" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_spell WHERE character_guid = $GINGER AND spell_id = $DEATHWISH_SPELL")" "1"

N0=$(sql1 "SELECT respec_count FROM game_character WHERE guid = $GINGER"); N0=${N0:-0}
# money moves on the LIVE entity mid-session (game_character.money persists at logout —
# the weaponmaster scenario documents the same rule); respec_count IS durable, read it from game_character.
MONEY0=$(sql1 "SELECT money FROM game_world_entity WHERE guid = $GINGER")
COST0=$(respec_cost "$N0")

scall debug_reset_talents "$GINGER" "$TRAINER"

assert_eq "reset 1: talent rows gone" "$(sql1 "SELECT COUNT(*) AS n FROM game_character_talent WHERE character_guid = $GINGER")" "0"
assert_eq "reset 1: Cruelty aura gone" "$(sql1 "SELECT COUNT(*) AS n FROM game_aura WHERE target_guid = $GINGER AND spell_id = $CRUELTY_AURA")" "0"
assert_eq "reset 1: Death Wish forgotten" "$(sql1 "SELECT COUNT(*) AS n FROM game_player_spell WHERE character_guid = $GINGER AND spell_id = $DEATHWISH_SPELL")" "0"
assert_eq "reset 1: money charged exactly cost($N0)=$COST0" "$(sql1 "SELECT money FROM game_world_entity WHERE guid = $GINGER")" "$((MONEY0 - COST0))"
assert_eq "reset 1: respec_count incremented" "$(sql1 "SELECT respec_count FROM game_character WHERE guid = $GINGER")" "$((N0 + 1))"

# Second reset — nothing learned in between: asserts the reducer tolerates an empty talent set
# (no rows/auras/spells to unwind) and still charges the NEXT escalated price off the persisted count.
MONEY1=$(sql1 "SELECT money FROM game_world_entity WHERE guid = $GINGER")
COST1=$(respec_cost "$((N0 + 1))")
scall debug_reset_talents "$GINGER" "$TRAINER"
assert_eq "reset 2: money charged exactly cost($((N0 + 1)))=$COST1" "$(sql1 "SELECT money FROM game_world_entity WHERE guid = $GINGER")" "$((MONEY1 - COST1))"
assert_eq "reset 2: respec_count incremented again" "$(sql1 "SELECT respec_count FROM game_character WHERE guid = $GINGER")" "$((N0 + 2))"

stay_stop

# teardown (asserted): the pad trainer only (the seeded start-area trainer stays)
sqlq "DELETE FROM game_creature_spawn WHERE entry = $TRAINER_ENTRY AND x > -8890" >/dev/null
sqlq "DELETE FROM game_world_entity WHERE guid = ${TRAINER:-0}" >/dev/null
assert_eq "teardown: pad trainer gone" "$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE guid = ${TRAINER:-0}")" "0"

if [ "$FAILED" -eq 0 ]; then echo "[test-respec] PASS"; exit 0; else echo "[test-respec] FAIL"; exit 1; fi
