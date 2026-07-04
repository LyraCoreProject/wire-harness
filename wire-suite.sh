#!/usr/bin/env bash
# wire-suite.sh — ONE command that runs EVERY headless wire probe + orchestrated test green against
# the live local node (work-item 139). This is the whole-game regression gate: implement-task verify
# agents SHOULD run it after their item-local proof, so a regression in an already-verified item
# fails the loop instead of waiting for luck.
#
#   bash tools/wire-client/wire-suite.sh              # full suite
#   WS_ONLY="who roll" bash tools/wire-client/wire-suite.sh   # subset by test name
#
# Design rules (the item's spec):
#   - per-test PASS/FAIL lines + a summary table; exit NONZERO on any FAIL.
#   - a probe that needs absent-in-sandbox data (DBC/cmangos-gated) is SKIPPED LOUDLY with the
#     reason — never silently. Skips do not fail the suite (they're environment facts, not bugs),
#     but they are printed in the summary so a shrinking test bed is visible.
#   - per-probe independence: every test opens its own wire session(s); one failure never cascades.
#   - fixtures are idempotent (mock-seed debug reducers + `gateway provision` + wire char-create);
#     stateful tests carry their own teardown (contact-list purge, post-death resurrect).
#
# Prereqs: local STDB node + gateway up (this script verifies both), module published WITH
# --features=debug_reducers (scripts/publish-module.sh), cargo able to build wire-client.
set -uo pipefail
cd "$(dirname "$0")/../.."

DB=spacetime-core
WC=./target/debug/wire-client
GW_PORT=8085
LOGDIR="${WS_LOGDIR:-/tmp/wire-suite}"
mkdir -p "$LOGDIR"

# ---------- tiny helpers ----------
# The canonical sqlq/char_guid/scall/stay_start/stay_stop (and sql assert_*) implementations live
# in scenario-lib.sh — ONE copy of the fiddly stay_start relogin-race settle/verify/retry, not a
# drifting suite-local fork (the fork that used to live here had already lost that race fix).
# SC_STAY_LOG_DIR makes the lib capture each stay session's wire log like the old fork did.
SC_STAY_LOG_DIR="$LOGDIR"
source tools/wire-client/scenario-lib.sh
# COUNT(*) helper — equality-filter only (spacetime sql 2.x range-filter gotcha, danger-zones §2).
countq() { sqlq "SELECT COUNT(*) AS n FROM $1" | grep -oE '[0-9]+' | tail -1; }
char_pos() { sqlq "SELECT x, y, z FROM game_character WHERE guid = $1" | tail -1; }

# ---------- result accounting ----------
PASS_N=0; FAIL_N=0; SKIP_N=0
RESULTS=() # "STATUS name reason-or-log"
SKIP_REASON="" # a test fn sets this then returns 77

run_test() { # $1 = test name (fn t_$1 must exist)
  local name=$1
  if [ -n "${WS_ONLY:-}" ] && ! grep -qw "$name" <<<"$WS_ONLY"; then return 0; fi
  SKIP_REASON=""
  local log="$LOGDIR/$name.log"
  echo "── [$name] running…"
  ( "t_$name" ) >"$log" 2>&1
  local rc=$?
  # a fixture/gate fn may export the reason via the log's first SKIP: line
  if [ $rc -eq 77 ]; then
    local reason; reason=$(grep -m1 '^SKIP:' "$log" | sed 's/^SKIP: //')
    echo "   SKIP  $name — ${reason:-no reason recorded (fix the test: skips must be loud)}"
    RESULTS+=("SKIP $name ${reason:-unspecified}")
    SKIP_N=$((SKIP_N + 1))
  elif [ $rc -eq 0 ]; then
    echo "   PASS  $name"
    RESULTS+=("PASS $name -")
    PASS_N=$((PASS_N + 1))
  else
    echo "   FAIL  $name (rc=$rc) — log: $log"
    tail -5 "$log" | sed 's/^/         | /'
    RESULTS+=("FAIL $name $log")
    FAIL_N=$((FAIL_N + 1))
  fi
}
skip() { echo "SKIP: $*"; exit 77; } # call from inside a test fn (subshell)

# ---------- preflight ----------
echo "[suite] preflight: node, gateway, wire-client build, accounts, characters…"
if ! sqlq "SELECT username FROM game_account" >/dev/null; then
  echo "[suite] FATAL: spacetime node/database '$DB' unreachable (is the local node up + module published?)" >&2
  exit 2
fi
if ! (exec 3<>"/dev/tcp/127.0.0.1/$GW_PORT") 2>/dev/null; then
  echo "[suite] FATAL: gateway world port $GW_PORT closed (start it per docs/danger-zones.md §3)" >&2
  exit 2
fi
cargo build -q -p wire-client || { echo "[suite] FATAL: wire-client build failed" >&2; exit 2; }

# Accounts: provision is operator-gated; tolerate "already provisioned", then ASSERT the row exists.
TOKEN=$(grep -oP 'spacetimedb_token = "\K[^"]+' ~/.config/spacetime/cli.toml || true)
for acct in TEST TEST2; do
  if [ -z "$(sqlq "SELECT username FROM game_account WHERE username = '$acct'" | grep -o "$acct")" ]; then
    env GW_COORDINATOR_TOKEN="$TOKEN" ./target/debug/gateway provision "$acct" test123 >/dev/null 2>&1 || true
  fi
  if [ -z "$(sqlq "SELECT username FROM game_account WHERE username = '$acct'" | grep -o "$acct")" ]; then
    echo "[suite] FATAL: account $acct missing and provision failed (operator claimed? token valid?)" >&2
    exit 2
  fi
done
# Characters: the wire client creates-on-login; `logout` mode is the cheapest clean round-trip.
[ -z "$(char_guid Ginger)" ]  && timeout 60 "$WC" TEST  test123 Ginger  logout >/dev/null 2>&1
[ -z "$(char_guid dfsdfsd)" ] && timeout 60 "$WC" TEST2 test123 dfsdfsd logout >/dev/null 2>&1
GINGER=$(char_guid Ginger); DFS=$(char_guid dfsdfsd)
if [ -z "$GINGER" ] || [ -z "$DFS" ]; then
  echo "[suite] FATAL: fixture characters missing (Ginger=$GINGER dfsdfsd=$DFS)" >&2
  exit 2
fi
echo "[suite] fixtures: Ginger=$GINGER dfsdfsd=$DFS"

# ---------- data-gate probes (sandbox vs fully-imported node) ----------
HAS_SPELL_686=$(countq "game_spell WHERE spell_id = 686")
HAS_CREATURE_103=$(countq "game_creature_template WHERE entry = 103")
HAS_COMBAT_REGEN_EFFECT=$(countq "game_spell_effect WHERE kind = 169")
HAS_FACTIONS=$(countq "game_faction")
HAS_LEVEL_STATS=$(countq "game_level_stats")
HAS_HEALER=$(countq "game_world_entity WHERE entry = 6491")

# ===================================================================================
#  Tests. Every wire-client probe mode + orchestrated .sh test is either RUN here or
#  LOUDLY SKIPPED with its data gate. (Modes stay/relay-observer/relay-sender are
#  infrastructure used inside orchestrated tests, not standalone tests; questgiver +
#  gossip are diagnostic dumps with no assertion — nothing to gate a PASS on.)
# ===================================================================================

t_logout()           { bash tools/wire-client/test-logout.sh; }
t_who()              { timeout 60 "$WC" TEST test123 Ginger who; }
t_roll()             { timeout 60 "$WC" TEST test123 Ginger roll 1 100; }
t_text_emote()       { timeout 60 "$WC" TEST test123 Ginger text-emote; }
t_played_time()      { timeout 60 "$WC" TEST test123 Ginger played-time; }
t_played_time_live() { timeout 60 "$WC" TEST test123 Ginger played-time-live 3; }
# decode-smoke: no ids asserted — on a no-import sandbox the starter book is loadout-fallback;
# the probe still proves SMSG_INITIAL_SPELLS arrives and decodes.
t_initial_spells()   { timeout 60 "$WC" TEST test123 Ginger initial-spells; }
# slot 15 = main-hand; the starter loadout grants a weapon even on a no-import node (Warrior fallback).
t_char_enum_gear()   { timeout 60 "$WC" TEST test123 Ginger char-enum-gear 15; }
# entry 25 (Worn Shortsword) is init-seeded everywhere; asserts the reply decodes with its known armor.
t_query_item()       { timeout 60 "$WC" TEST test123 Ginger query-item 25 0; }
t_char_delete()      { timeout 60 "$WC" TEST test123 Wsthrowaway char-delete; }

t_bindpoint() {
  # Fixture: home at A, then move to B and logout — SMSG_BINDPOINTUPDATE must carry A (not B).
  local AX=-8873.0 AY=-134.0 AZ=81.0 BX=-8968.0 BY=-129.0 BZ=83.4
  stay_start TEST test123 Ginger || exit 1
  spacetime call "$DB" -- debug_teleport "$GINGER" 0 $AX $AY $AZ 0 || { stay_stop; exit 1; }
  spacetime call "$DB" -- debug_bind_home "$GINGER" || { stay_stop; exit 1; }
  spacetime call "$DB" -- debug_teleport "$GINGER" 0 $BX $BY $BZ 0 || { stay_stop; exit 1; }
  stay_stop
  timeout 60 "$WC" TEST test123 Ginger bindpoint $AX $AY $AZ
}

t_inspect() {
  # dfsdfsd parked <10yd from Ginger's stored position; far guid is a nonexistent player.
  local pos gx gy gz
  pos=$(char_pos "$GINGER"); gx=$(awk -F'|' '{print $1+0}' <<<"$pos"); gy=$(awk -F'|' '{print $2+0}' <<<"$pos"); gz=$(awk -F'|' '{print $3+0}' <<<"$pos")
  stay_start TEST2 test123 dfsdfsd || exit 1
  spacetime call "$DB" -- debug_teleport "$DFS" 0 "$(awk "BEGIN{print $gx+3}")" "$gy" "$gz" 0 || { stay_stop; exit 1; }
  timeout 60 "$WC" TEST test123 Ginger inspect "$DFS" 999999999
  local rc=$?
  stay_stop
  return $rc
}

t_friend() {
  # friend-list Online assertion needs the target connected — park dfsdfsd in a stay session.
  stay_start TEST2 test123 dfsdfsd || exit 1
  timeout 60 "$WC" TEST test123 Ginger friend dfsdfsd
  local rc=$?
  stay_stop
  return $rc
}

t_ignore_whisper() {
  # The probe asserts a strict IgnoreAdded, so purge Ginger's contact rows first (repeatability),
  # and leave them purged after (the friend test tolerates Already, but a stale ignore row would
  # silently eat unrelated whisper traffic in later tests).
  sqlq "DELETE FROM game_character_contact WHERE owner_guid = $GINGER" >/dev/null
  timeout 90 "$WC" TEST test123 Ginger ignore-whisper TEST2 test123 dfsdfsd
  local rc=$?
  sqlq "DELETE FROM game_character_contact WHERE owner_guid = $GINGER" >/dev/null
  return $rc
}

position_apart() { # say-range/move-relay geometry: ~32yd apart (out of 25yd SAY, inside 125yd AOI)
  stay_start TEST test123 Ginger || return 1
  spacetime call "$DB" -- debug_teleport "$GINGER" 0 -8968 -129 83.4 0 || { stay_stop; return 1; }
  stay_stop
  stay_start TEST2 test123 dfsdfsd || return 1
  spacetime call "$DB" -- debug_teleport "$DFS" 0 -8945 -107 83.4 0 || { stay_stop; return 1; }
  stay_stop
}

t_say_range()  { position_apart || exit 1; bash tools/wire-client/test-say-range.sh; }
t_move_relay() { position_apart || exit 1; bash tools/wire-client/test-move-relay.sh; }

t_persist_health() {
  bash tools/wire-client/test-persist-health.sh
  local rc=$?
  # teardown: heal the 22hp residue so later combat tests don't start near-dead
  stay_start TEST test123 Ginger && spacetime call "$DB" -- debug_set_health "$GINGER" 100000; stay_stop
  return $rc
}

t_repop_delay() {
  bash tools/wire-client/test-repop-delay.sh
  local rc=$?
  # teardown: the test leaves Ginger dead/ghost — resurrect + heal for whatever runs next
  stay_start TEST test123 Ginger || return $rc
  spacetime call "$DB" -- debug_spirit_healer_res "$GINGER" >/dev/null 2>&1 || true
  spacetime call "$DB" -- debug_set_health "$GINGER" 100000 >/dev/null 2>&1 || true
  stay_stop
  return $rc
}

t_ding()         { bash tools/wire-client/test-ding.sh; }
t_combat_regen() {
  # The probe needs a spell whose effect is kind 169 (A_COMBAT_HEALTH_REGEN_PCT). The 092-era
  # fixture rode on Demon Skin 696 until work-item 024 reclassified its regen to A_PERIODIC_HEAL;
  # a long-lived node kept the stale 169 row (insert-if-absent seeds), a fresh node has NONE — so
  # the probe is data-gated exactly like the DBC-import probes (work-item filed to restore a
  # kind-169 fixture, e.g. the Troll Regeneration racial).
  [ "${HAS_COMBAT_REGEN_EFFECT:-0}" -ge 1 ] \
    || skip "no kind-169 (A_COMBAT_HEALTH_REGEN_PCT) spell effect on this node — the 092 fixture went with 024's Demon-Skin reclassification; needs a kind-169 source (Troll Regeneration import or a new fixture)"
  bash tools/wire-client/test-combat-regen.sh
}

t_cast_flow() {
  [ "${HAS_SPELL_686:-0}" -ge 1 ] && [ "${HAS_CREATURE_103:-0}" -ge 1 ] \
    || skip "spell 686 (Shadow Bolt) / creature 103 not imported — needs the cmangos+DBC world import (scripts/import-world.sh), absent on a mock-seed sandbox"
  bash tools/wire-client/test-cast-flow.sh
}

t_cast_interrupt() {
  [ "${HAS_SPELL_686:-0}" -ge 1 ] && [ "${HAS_CREATURE_103:-0}" -ge 1 ] \
    || skip "spell 686 (Shadow Bolt) / creature 103 not imported — needs the cmangos+DBC world import, absent on a mock-seed sandbox"
  bash tools/wire-client/test-cast-interrupt.sh
}

t_ghost_reveal() {
  [ "${HAS_HEALER:-0}" -ge 1 ] || skip "no spirit healer (entry 6491) spawned — world-import-gated; test also requires a GW_AOI=0 gateway (isolates the on_update reveal path)"
  [ "${GW_AOI:-1}" = "0" ] || skip "gateway running with GW_AOI=1 — this test isolates the on_update reveal and needs GW_AOI=0 (see its header)"
  bash tools/wire-client/test-ghost-reveal.sh
}

t_init_factions() {
  # Uses the scenario-fixture faction 79 (reputation_index 5 — seeded by debug_seed_scenario_fixtures,
  # present even on a no-import sandbox). Standing accumulates across runs (quest rewards + this
  # grant), so read the expected value back instead of hardcoding it.
  spacetime call "$DB" -- debug_seed_scenario_fixtures >/dev/null 2>&1 || true
  spacetime call "$DB" -- debug_grant_reputation "$GINGER" 79 100 >/dev/null 2>&1 || true
  local want
  want=$(sqlq "SELECT standing FROM game_player_reputation WHERE character_guid = $GINGER AND faction_id = 79" | grep -oE '\-?[0-9]+' | tail -1)
  [ -z "$want" ] && skip "debug_grant_reputation left no game_player_reputation row for fixture faction 79"
  timeout 60 "$WC" TEST test123 Ginger init-factions 5 "$want"
}

t_levelup_info() {
  [ "${HAS_LEVEL_STATS:-0}" -ge 1 ] || skip "game_level_stats empty — the cmangos stat curve isn't imported, so ding deltas are all zero and the probe's non-zero-stat assertion can't hold"
  # orchestrate: stage at L1 + boosted XP, kill a seeded wolf, expect SMSG_LEVELUP_INFO
  # Run-scoped handshake path (work-item 161): defined once, passed as the levelup-info ready arg.
  local ready=/tmp/wc_levelup_ready_$$
  rm -f "$ready"
  spacetime call "$DB" -- debug_set_level "$GINGER" 1 >/dev/null 2>&1 || true
  spacetime call "$DB" -- debug_set_xp_rate 100 >/dev/null 2>&1 || true
  (
    for _ in $(seq 1 30); do [ -f "$ready" ] && break; sleep 1; done
    rm -f "$ready"
    spacetime call "$DB" -- debug_spawn_at_feet "$GINGER" 51000 5 >/dev/null 2>&1
    sleep 1
    spacetime call "$DB" -- debug_kill_nearest "$GINGER" 51000 >/dev/null 2>&1
  ) &
  local orch=$!
  timeout 90 "$WC" TEST test123 Ginger levelup-info "$ready" 1
  local rc=$?
  wait "$orch" 2>/dev/null
  spacetime call "$DB" -- debug_set_xp_rate 1 >/dev/null 2>&1 || true
  return $rc
}

# ---- multi-client AOI/relay regression + soak (work-item 141) ----
t_aoi_relay() {
  [ "${GW_AOI:-1}" = "1" ] || skip "gateway must run with GW_AOI=1 (grid-scoped subscriptions) for the AOI boundary assertions"
  bash tools/wire-client/test-aoi-relay.sh
}
# Suite gate runs a 60s soak (SOAK_SECS overrides); the >=10-minute acceptance run is recorded in
# the 141 resolution — a 10-minute wait per suite iteration would make the gate impractical.
t_soak() { SOAK_SECS="${SOAK_SECS:-60}" bash tools/wire-client/test-soak.sh; }

# ---- scenario runner (work-item 140): the four multi-step gameplay flows ----
t_scenario_quest()  { bash tools/wire-client/test-scenario-quest.sh; }
t_scenario_vendor() { bash tools/wire-client/test-scenario-vendor.sh; }
t_scenario_train()  { bash tools/wire-client/test-scenario-train.sh; }
t_scenario_death()  { bash tools/wire-client/test-scenario-death.sh; }

# ---- party/group system (work-item 066): two-session invite/accept/list/xp-split/quest-credit/
# range-gate/disband/decline acceptance ----
t_group() { bash tools/wire-client/test-group.sh; }
t_party_brains() { bash tools/wire-client/test-party-brains.sh; }
t_bot_goals() { bash tools/wire-client/test-bot-goals.sh; }
t_class_roles() { bash tools/wire-client/test-class-roles.sh; }

# ---- playerbots package acceptance (work-item 142) — the script self-SKIPs (exit 77) when the
# packages/playerbots drop-in isn't installed/published. ----
t_playerbots() { bash tools/wire-client/test-playerbots.sh; }

# ---------- the run ----------
ALL_TESTS=(
  logout who roll text_emote played_time played_time_live initial_spells char_enum_gear
  query_item char_delete bindpoint inspect friend ignore_whisper say_range move_relay
  persist_health repop_delay ding combat_regen cast_flow cast_interrupt ghost_reveal
  init_factions levelup_info
  scenario_quest scenario_vendor scenario_train scenario_death
  aoi_relay soak playerbots group party_brains bot_goals class_roles
)
START=$(date +%s)
for t in "${ALL_TESTS[@]}"; do run_test "$t"; done

echo
echo "════════ wire-suite summary ($(( $(date +%s) - START ))s) ════════"
for r in "${RESULTS[@]}"; do
  status=${r%% *}; rest=${r#* }; name=${rest%% *}; info=${rest#* }
  case $status in
    PASS) printf "  PASS  %s\n" "$name" ;;
    SKIP) printf "  SKIP  %-18s %s\n" "$name" "$info" ;;
    FAIL) printf "  FAIL  %-18s log: %s\n" "$name" "$info" ;;
  esac
done
echo "──────────────────────────────────────────────"
echo "  $PASS_N passed, $FAIL_N failed, $SKIP_N skipped (skip = missing sandbox data, reason above)"
if [ "$FAIL_N" -gt 0 ]; then
  echo "  RESULT: FAIL"
  exit 1
fi
echo "  RESULT: GREEN"
exit 0
