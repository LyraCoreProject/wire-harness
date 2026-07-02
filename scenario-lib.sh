#!/usr/bin/env bash
# Shared helpers for the test-scenario-*.sh orchestrators (work-item 140). Source, don't run.
# Convention: every orchestrator (1) seeds the idempotent scenario fixtures, (2) stages NPCs at a
# per-scenario pad via a short stay session, (3) runs the wire-client scenario mode, (4) sql-asserts
# server state at each seam, (5) tears down every spawned row and ASSERTS the teardown.

DB=spacetime-core
WC=./target/debug/wire-client

sqlq() { spacetime sql "$DB" "$1" 2>/dev/null; }
scall() { spacetime call "$DB" -- "$@" >/dev/null 2>&1; }
char_guid() { sqlq "SELECT guid FROM game_character WHERE name = '$1'" | grep -oE '[0-9]+' | tail -1; }
# first numeric column of the first data row
sql1() { sqlq "$1" | sed -n 3p | awk -F'|' '{gsub(/ /,"",$1); print $1}'; }

FAILED=0
step_ok()   { echo "[orch] STEP-ASSERT OK: $*"; }
step_fail() { echo "[orch] STEP-ASSERT FAIL: $*" >&2; FAILED=1; }
assert_eq() { # $1=label $2=got $3=want
  if [ "$2" = "$3" ]; then step_ok "$1 ($2)"; else step_fail "$1: got '$2' want '$3'"; fi
}
assert_ge() { if [ -n "$2" ] && [ "$2" -ge "$3" ] 2>/dev/null; then step_ok "$1 ($2 >= $3)"; else step_fail "$1: got '$2' want >= $3"; fi; }
assert_gt() { if [ -n "$2" ] && [ "$2" -gt "$3" ] 2>/dev/null; then step_ok "$1 ($2 > $3)"; else step_fail "$1: got '$2' want > $3"; fi; }
assert_lt() { if [ -n "$2" ] && [ "$2" -lt "$3" ] 2>/dev/null; then step_ok "$1 ($2 < $3)"; else step_fail "$1: got '$2' want < $3"; fi; }

SC_STAY_PID=""; SC_STAY_SENTINEL=""
stay_start() { # $1=account $2=pass $3=char
  local guid attempt; guid=$(char_guid "$3")
  SC_STAY_SENTINEL="/tmp/sc_stay_$$_$3"
  # A fast relogin can race the PREVIOUS session's disconnect cleanup (which despawns the character
  # guid and would delete the NEW session's entity from under us) — settle first, then verify the
  # entity SURVIVES a beat after appearing, retrying the whole login once if it got reaped.
  sleep 2
  for attempt in 1 2; do
    rm -f "$SC_STAY_SENTINEL"
    "$WC" "$1" "$2" "$3" stay "$SC_STAY_SENTINEL" >/dev/null 2>&1 &
    SC_STAY_PID=$!
    local live=""
    for _ in $(seq 1 20); do
      live=$(sqlq "SELECT guid FROM game_world_entity WHERE guid = $guid" | grep -oE '[0-9]+' | tail -1)
      [ -n "$live" ] && break
      sleep 1
    done
    if [ -n "$live" ]; then
      sleep 2
      if [ -n "$(sqlq "SELECT guid FROM game_world_entity WHERE guid = $guid" | grep -oE '[0-9]+' | tail -1)" ]; then
        return 0
      fi
      echo "[orch] stay_start: $3 entity reaped by a stale disconnect (attempt $attempt) — retrying" >&2
    fi
    touch "$SC_STAY_SENTINEL"; wait "$SC_STAY_PID" 2>/dev/null; sleep 2
  done
  echo "[orch] stay_start: $3 never went live" >&2
  return 1
}
stay_stop() {
  [ -n "$SC_STAY_SENTINEL" ] && touch "$SC_STAY_SENTINEL"
  [ -n "$SC_STAY_PID" ] && wait "$SC_STAY_PID" 2>/dev/null
  SC_STAY_PID=""; SC_STAY_SENTINEL=""
  sleep 1
}

# Spawn `entry` at a live character's feet and echo the NEW entity guid (the highest guid of that
# entry — debug_spawn_at_feet allocates a fresh, strictly higher spawn guid per call).
spawn_at() { # $1=anchor_char_guid $2=entry $3=offset
  scall debug_spawn_at_feet "$1" "$2" "$3" || return 1
  sleep 1
  sqlq "SELECT guid FROM game_world_entity WHERE entry = $2" | grep -oE '[0-9]{15,}' | sort -n | tail -1
}

# Remove every live entity + spawn row + corpse-loot residue of a fixture entry, then assert zero.
purge_entry() { # $1=entry
  sqlq "DELETE FROM game_creature_spawn WHERE entry = $1" >/dev/null
  sqlq "DELETE FROM game_world_entity WHERE entry = $1 AND owner_guid = 0" >/dev/null
  local left
  left=$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE entry = $1")
  assert_eq "teardown: no live entities of entry $1 remain" "${left:-0}" "0"
  left=$(sql1 "SELECT COUNT(*) AS n FROM game_creature_spawn WHERE entry = $1")
  assert_eq "teardown: no spawn rows of entry $1 remain" "${left:-0}" "0"
}

scenario_preflight() { # $1=scenario name
  cargo build -q -p wire-client || { echo "[orch] wire-client build failed" >&2; exit 1; }
  scall debug_seed_scenario_fixtures || true
  GINGER=$(char_guid Ginger)
  [ -z "$GINGER" ] && timeout 60 "$WC" TEST test123 Ginger logout >/dev/null 2>&1 && GINGER=$(char_guid Ginger)
  [ -z "$GINGER" ] && { echo "[orch] no Ginger character" >&2; exit 1; }
  echo "[orch] $1: Ginger=$GINGER"
}
