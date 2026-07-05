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
  # Callers that want the wire session's output (the suite) set SC_STAY_LOG_DIR; default discards.
  local staylog="${SC_STAY_LOG_DIR:+${SC_STAY_LOG_DIR}/stay_$3.log}"
  for attempt in 1 2; do
    rm -f "$SC_STAY_SENTINEL"
    "$WC" "$1" "$2" "$3" stay "$SC_STAY_SENTINEL" >"${staylog:-/dev/null}" 2>&1 &
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

# Silently remove every spawn row + live entity + corpse-loot residue of a fixture entry.
# PER GUID: a compound-WHERE DELETE silently no-ops on this CLI (the 146 lesson) — the old
# entry+owner_guid form here left stale entities behind and three tests grew local copies of the
# fix; this is now the one canonical purge.
purge_entry_rows() { # $1=entry
  local G
  for G in $(sqlq "SELECT guid FROM game_creature_spawn WHERE entry = $1" | grep -oE '[0-9]{6,}'); do
    sqlq "DELETE FROM game_creature_spawn WHERE guid = $G" >/dev/null
  done
  for G in $(sqlq "SELECT guid FROM game_world_entity WHERE entry = $1" | grep -oE '[0-9]{6,}'); do
    sqlq "DELETE FROM game_world_entity WHERE guid = $G" >/dev/null
    sqlq "DELETE FROM game_corpse_loot WHERE corpse_guid = $G" >/dev/null
  done
}

# Purge a fixture entry AND assert nothing remains — the asserting teardown form.
purge_entry() { # $1=entry
  purge_entry_rows "$1"
  local left
  left=$(sql1 "SELECT COUNT(*) AS n FROM game_world_entity WHERE entry = $1")
  assert_eq "teardown: no live entities of entry $1 remain" "${left:-0}" "0"
  left=$(sql1 "SELECT COUNT(*) AS n FROM game_creature_spawn WHERE entry = $1")
  assert_eq "teardown: no spawn rows of entry $1 remain" "${left:-0}" "0"
}

# Proximity-aggro hostility on a mock-seed sandbox: no faction data is imported and
# compute_hostile refuses missing rows, so nothing can aggro without staging these two
# game_faction_template rows (Monster 14 enemy-group vs Player group 1). ALWAYS pair with
# clear_hostility in teardown — the rest of the suite expects the pre-import baseline.
stage_hostility() {
  sqlq "INSERT INTO game_faction_template (id, faction, faction_group, friend_group, enemy_group, enemy_0, enemy_1, enemy_2, enemy_3, friend_0, friend_1, friend_2, friend_3) VALUES (14, 14, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0)" >/dev/null
  sqlq "INSERT INTO game_faction_template (id, faction, faction_group, friend_group, enemy_group, enemy_0, enemy_1, enemy_2, enemy_3, friend_0, friend_1, friend_2, friend_3) VALUES (1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0)" >/dev/null
}
clear_hostility() {
  sqlq "DELETE FROM game_faction_template WHERE id = 14" >/dev/null
  sqlq "DELETE FROM game_faction_template WHERE id = 1" >/dev/null
}

# Poll once per second for up to $1 seconds until file $2 exists. Echoes nothing; returns 0 when
# the file appears, 1 on timeout — the ONE copy of the hand-rolled `for _ in $(seq 1 N); do [ -f
# FILE ] && break; sleep 1; done` handshake poll. The caller keeps its own error message:
# `wait_for_file 30 "$READY" || { echo "[orch] never ready" >&2; exit 1; }`.
wait_for_file() { # $1=secs $2=path
  local i
  for i in $(seq 1 "$1"); do
    [ -f "$2" ] && return 0
    sleep 1
  done
  return 1
}

# Observer cmd/ack file protocol (the aoi-observer wire mode): clear the ack, write the command,
# wait up to $4 (default 35) seconds for the ack file, then echo its content (empty on timeout).
# Mechanics ONLY — the CALLER interprets OK/FAIL in the echoed ack and does its own
# step_ok/step_fail (or rc) reporting, which intentionally differs per test.
obs_cmd_send() { # $1=cmd $2=cmd_file $3=ack_file [$4=secs]
  rm -f "$3"
  echo "$1" > "$2"
  wait_for_file "${4:-35}" "$3"
  cat "$3" 2>/dev/null
}

# Canonical say-range/move-relay geometry: park the STORED rows of Ginger (TEST) and dfsdfsd
# (TEST2) ~32yd apart — outside the 25yd SAY range, inside the 125yd AOI box — via short stay
# sessions (login places each session at its stored character position). Owned by
# test-say-range.sh/test-move-relay.sh so standalone runs stage the same fixture as suite runs.
position_apart() {
  local G D
  G=$(char_guid Ginger); D=$(char_guid dfsdfsd)
  { [ -n "$G" ] && [ -n "$D" ]; } || { echo "[orch] position_apart: missing fixture character (Ginger=$G dfsdfsd=$D)" >&2; return 1; }
  stay_start TEST test123 Ginger || return 1
  scall debug_teleport "$G" 0 -8968 -129 83.4 0 || { stay_stop; return 1; }
  stay_stop
  stay_start TEST2 test123 dfsdfsd || return 1
  scall debug_teleport "$D" 0 -8945 -107 83.4 0 || { stay_stop; return 1; }
  stay_stop
}

# Poll a 1-value sql query once per second for up to $1 seconds until it compares true against
# $3 (eq/ge). Echoes nothing; returns 0 on success, 1 on timeout — `wait_for_sql_eq 30 "SELECT
# ..." 4 && step_ok ... || step_fail ...` replaces the hand-rolled seq/sleep loops.
wait_for_sql_eq() { # $1=secs $2=query $3=want
  local i v
  for i in $(seq 1 "$1"); do
    v=$(sql1 "$2"); [ "${v:-}" = "$3" ] && return 0
    sleep 1
  done
  return 1
}
wait_for_sql_ge() { # $1=secs $2=query $3=want
  local i v
  for i in $(seq 1 "$1"); do
    v=$(sql1 "$2"); [ "${v:-0}" -ge "$3" ] 2>/dev/null && return 0
    sleep 1
  done
  return 1
}

scenario_preflight() { # $1=scenario name
  cargo build -q -p wire-client || { echo "[orch] wire-client build failed" >&2; exit 1; }
  scall debug_seed_scenario_fixtures || true
  GINGER=$(char_guid Ginger)
  [ -z "$GINGER" ] && timeout 60 "$WC" TEST test123 Ginger logout >/dev/null 2>&1 && GINGER=$(char_guid Ginger)
  [ -z "$GINGER" ] && { echo "[orch] no Ginger character" >&2; exit 1; }
  echo "[orch] $1: Ginger=$GINGER"
}
