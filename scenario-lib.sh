#!/usr/bin/env bash
# Shared helpers for the test-scenario-*.sh orchestrators (work-item 140). Source, don't run.
# Convention: every orchestrator (1) seeds the idempotent scenario fixtures, (2) stages NPCs at a
# per-scenario pad via a short stay session, (3) runs the wire-client scenario mode, (4) sql-asserts
# server state at each seam, (5) tears down every spawned row and ASSERTS the teardown.

DB=spacetime-core
WC=./target/debug/wire-client

# $2/db is optional on every read/lookup helper below (default: the $DB global) — issue #70: a
# character living on a NON-default database (e.g. an instance or continent shard) used to read as
# "never went live" because every helper was hardwired to the single global. Pass a db explicitly
# only where a caller genuinely needs one shard's answer while $DB names another (see
# test-transfer-crash-matrix.sh's bring_home for the motivating case); every existing call site is
# unchanged because the parameter defaults to $DB when omitted.
sqlq() { spacetime sql "${2:-$DB}" "$1" 2>/dev/null; } # $1=query $2=database (optional)
scall() { spacetime call "$DB" -- "$@" >/dev/null 2>&1; }
char_guid() { sqlq "SELECT guid FROM game_character WHERE name = '$1'" "$2" | grep -oE '[0-9]+' | tail -1; } # $1=name $2=database (optional)
# first numeric column of the first data row
sql1() { sqlq "$1" | sed -n 3p | awk -F'|' '{gsub(/ /,"",$1); print $1}'; }

# Wait for the char row's money to go QUIET (two consecutive 1s-apart equal reads). The gateway's
# logout persist is ASYNC (~1-3s; danger-zones §2): an offline write (debug_set_money) or a delta
# BASELINE taken while a prior session's persist is still in flight gets silently clobbered/skewed —
# the 265 trace caught a staged 1000 overwritten by the previous session's late 800 persist. Call
# before any debug_set_money and before taking money/xp baselines. (267)
settle_char_money() { # $1=char_guid
  local prev="" cur=""
  for _ in $(seq 1 10); do
    cur=$(sql1 "SELECT money FROM game_character WHERE guid = $1")
    [ -n "$cur" ] && [ "$cur" = "$prev" ] && return 0
    prev="$cur"; sleep 1
  done
  return 0 # proceed either way — the caller's own asserts report the truth
}

# Disposable per-test character (testing-hardening §3.4 — kills the shared-Ginger ambient-state
# flake class, work-item 267): delete-first (reaps a crashed prior run's leftover), create fresh
# through the REAL wire char-create path, echo the guid. Pair with drop_char in teardown. WC must
# be defined by the caller before use (every scenario script sets it).
fresh_char() { # $1=name  $2=class (warrior|...; default warrior)  $3=level (default 5)
  timeout 60 "$WC" TEST test123 "$1" char-delete >/dev/null 2>&1 || true
  timeout 60 "$WC" TEST test123 "$1" make-char "${2:-warrior}" >/dev/null 2>&1
  local guid; guid=$(char_guid "$1")
  # Level 5 default (267 root-cause, 2026-07-16): the scenario pads sit near REAL imported
  # low-level spawns — a level-1 fresh char is a valid aggro target and gets mauled mid-scenario
  # (4 wolves killed Vendortester during the vendor fight; death disengage cleared every melee row
  # and the durability assert starved). Level 5 is GREY to them (no proximity aggro) — the exact
  # profile the shared Ginger accidentally provided all along.
  [ -n "$guid" ] && scall debug_set_level "$guid" "${3:-5}"
  echo "$guid"
}
drop_char() { # $1=name
  timeout 60 "$WC" TEST test123 "$1" char-delete >/dev/null 2>&1 || true
}

# Clear the pad of REAL imported hostiles before a controlled fight (2026-07-18). The Elwynn scenario
# pads sit in real-mob territory (e.g. Defias Thugs entry 38 patrol near -8960,-420) — a patrolling
# mob wanders onto the pad and aggros the level-5 disposable char, stalling the controlled fight
# (the char fights the stray, not the bag). Delete live CREATURE entities (guid is the 0xF130 high-guid,
# always > 1e15; players are small guids) within `radius` yd of the point. Entity-only delete — real
# spawns respawn from their game_creature_spawn rows after the test. SQL has no distance fn, so filter
# in awk.
purge_creatures_near() { # $1=x $2=y $3=radius [$4... = guids to KEEP (fixtures: the bag, vendor, ...)]
  local px="$1" py="$2" r="$3"; shift 3
  local keep=" $* " g
  for g in $(sqlq "SELECT guid, x, y FROM game_world_entity" \
      | awk -F'|' -v px="$px" -v py="$py" -v r="$r" 'NR>2 {
          gsub(/ /,"",$1); gid=$1; ex=$2+0; ey=$3+0;
          if (gid+0 > 1000000000000000 && (ex-px)*(ex-px)+(ey-py)*(ey-py) <= r*r) print gid
        }'); do
    case "$keep" in *" $g "*) continue;; esac   # never delete a named fixture
    sqlq "DELETE FROM game_melee_attack WHERE attacker_guid = $g" >/dev/null 2>&1
    sqlq "DELETE FROM game_world_entity WHERE guid = $g" >/dev/null 2>&1
  done
}

FAILED=0
step_ok()   { echo "[orch] STEP-ASSERT OK: $*"; }
step_fail() { echo "[orch] STEP-ASSERT FAIL: $*" >&2; FAILED=1; }
assert_eq() { # $1=label $2=got $3=want
  if [ "$2" = "$3" ]; then step_ok "$1 ($2)"; else step_fail "$1: got '$2' want '$3'"; fi
}
assert_ge() { if [ -n "$2" ] && [ "$2" -ge "$3" ] 2>/dev/null; then step_ok "$1 ($2 >= $3)"; else step_fail "$1: got '$2' want >= $3"; fi; }
assert_gt() { if [ -n "$2" ] && [ "$2" -gt "$3" ] 2>/dev/null; then step_ok "$1 ($2 > $3)"; else step_fail "$1: got '$2' want > $3"; fi; }
assert_lt() { if [ -n "$2" ] && [ "$2" -lt "$3" ] 2>/dev/null; then step_ok "$1 ($2 < $3)"; else step_fail "$1: got '$2' want < $3"; fi; }

# Wait for a backgrounded wire-client PID to exit, with a shell-side deadline of its own. A plain
# `wait $PID` trusts the client's own self-exit contract (stay_start's $4/deadline_secs below) with
# no floor here — a client that never notices the sentinel or fails to honor its own deadline would
# hang the CALLER forever (issue #70 deadline audit: this was the one un-bounded wait in the file).
# Budget past the requested deadline, then SIGKILL and proceed rather than hang.
#
# review (issue #70): `kill -0`/`kill -9` operate on a bare pid number — they do not check that it is
# still OUR wire-client. A pid this shell has already reaped (via a PRIOR call to this function) can
# be recycled by the OS for an unrelated process; if that recycled pid is then handed back in here a
# second time, the budget would elapse waiting on a process we have no relationship to and then
# SIGKILL it. This is not hypothetical: stay_start's own retry-teardown below already calls this
# function once per failed attempt, and callers that don't check stay_start's return code
# (test-scenario-death.sh's `stay_start ... || FAILED=1` followed unconditionally by `stay_stop`, and
# test-scenario-vendor.sh's `stay_start ... && scall ...` followed unconditionally by `stay_stop`, are
# two live examples already in this tree) then run stay_stop against that same, already-reaped
# SC_STAY_PID. Two independent defenses: (1) stay_start now clears SC_STAY_PID immediately after this
# function reaps it, so a later unconditional stay_stop sees nothing to do (see below); (2) as a
# belt-and-suspenders check here too, refuse the kill if the pid no longer looks like our client.
_stay_wait_exit() { # $1=pid $2=budget_secs
  local i
  for i in $(seq 1 "$2"); do
    kill -0 "$1" 2>/dev/null || { wait "$1" 2>/dev/null; return 0; }
    sleep 1
  done
  if ! ps -o comm= -p "$1" 2>/dev/null | grep -q '^wire-client$'; then
    echo "[orch] stay: pid $1 did not exit within ${2}s, but no longer looks like our wire-client" \
         "(stale/reused pid) — refusing to kill it" >&2
    return 0
  fi
  echo "[orch] stay: wire-client pid $1 did not exit within ${2}s — killing it" >&2
  kill -9 "$1" 2>/dev/null
  wait "$1" 2>/dev/null
}

SC_STAY_PID=""; SC_STAY_SENTINEL=""; SC_STAY_DEADLINE=""
stay_start() { # $1=account $2=pass $3=char $4=deadline_secs (optional, default 60) $5=database (optional, default $DB)
  # deadline_secs (work-item 157): the wire client's stay mode self-exits (rc 0, silent) once its
  # own deadline elapses, regardless of whether stay_stop has been called yet. A caller that holds
  # the session live across a longer poll window than 60s (scenario-vendor's Step 4 fight-durability
  # poll runs up to 120s) MUST pass a deadline that exceeds that window, or the session — and the
  # character's live connection — can vanish mid-poll.
  # $5/database (issue #70): lets a caller materialise a session and poll for its entity on a
  # database OTHER than $DB (e.g. an instance/continent shard) without reassigning the global.
  local db="${5:-$DB}"
  local guid attempt; guid=$(char_guid "$3" "$db")
  local deadline="${4:-60}"
  SC_STAY_DEADLINE="$deadline"
  SC_STAY_SENTINEL="/tmp/sc_stay_$$_$3"
  # A fast relogin can race the PREVIOUS session's disconnect cleanup (which despawns the character
  # guid and would delete the NEW session's entity from under us) — settle first, then verify the
  # entity SURVIVES a beat after appearing, retrying the whole login once if it got reaped.
  sleep 2
  # Callers that want the wire session's output (the suite) set SC_STAY_LOG_DIR; default discards.
  local staylog="${SC_STAY_LOG_DIR:+${SC_STAY_LOG_DIR}/stay_$3.log}"
  for attempt in 1 2; do
    rm -f "$SC_STAY_SENTINEL"
    "$WC" "$1" "$2" "$3" stay "$SC_STAY_SENTINEL" "$deadline" >"${staylog:-/dev/null}" 2>&1 &
    SC_STAY_PID=$!
    local live=""
    for _ in $(seq 1 20); do
      live=$(sqlq "SELECT guid FROM game_world_entity WHERE guid = $guid" "$db" | grep -oE '[0-9]+' | tail -1)
      [ -n "$live" ] && break
      sleep 1
    done
    if [ -n "$live" ]; then
      sleep 2
      if [ -n "$(sqlq "SELECT guid FROM game_world_entity WHERE guid = $guid" "$db" | grep -oE '[0-9]+' | tail -1)" ]; then
        return 0
      fi
      echo "[orch] stay_start: $3 entity reaped by a stale disconnect (attempt $attempt) — retrying" >&2
    fi
    # Clear SC_STAY_PID as soon as this attempt's client is reaped (issue #70 review): if this was
    # the LAST attempt, stay_start returns 1 below with SC_STAY_PID now empty, so a caller that calls
    # stay_stop unconditionally after a failed stay_start (two such callers already exist — see
    # _stay_wait_exit's comment) finds nothing to wait on/kill, instead of re-using this attempt's
    # now-stale, already-reaped pid.
    touch "$SC_STAY_SENTINEL"; _stay_wait_exit "$SC_STAY_PID" $(( deadline + 15 )); SC_STAY_PID=""; sleep 2
  done
  echo "[orch] stay_start: $3 never went live" >&2
  return 1
}
stay_stop() {
  [ -n "$SC_STAY_SENTINEL" ] && touch "$SC_STAY_SENTINEL"
  [ -n "$SC_STAY_PID" ] && _stay_wait_exit "$SC_STAY_PID" $(( ${SC_STAY_DEADLINE:-60} + 15 ))
  SC_STAY_PID=""; SC_STAY_SENTINEL=""; SC_STAY_DEADLINE=""
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
# clear_hostility in teardown.
#
# IMPORTED-NODE GUARD (live find 2026-07-16): on a node with the real FactionTemplate.dbc import,
# rows 14 and 1 ALREADY EXIST with the real values — the old unconditional INSERT+DELETE replaced
# the imported Monster/Player templates with these sandbox stubs and then deleted them outright,
# corrupting live faction data until the next `importer --dbc` pass. The real rows already encode
# exactly the hostility this stages (Monster 14 enemy_group=1 → hostile to players), so on an
# imported node both helpers are no-ops; STAGED_HOSTILITY makes clear_hostility remove only what
# stage_hostility actually inserted.
STAGED_HOSTILITY=0
stage_hostility() {
  if [ -n "$(sql1 "SELECT COUNT(*) AS n FROM game_faction_template WHERE id = 14")" ] && \
     [ "$(sql1 "SELECT COUNT(*) AS n FROM game_faction_template WHERE id = 14")" != "0" ]; then
    return 0 # imported node: the real Monster template already provides the hostility
  fi
  STAGED_HOSTILITY=1
  sqlq "INSERT INTO game_faction_template (id, faction, faction_group, friend_group, enemy_group, enemy_0, enemy_1, enemy_2, enemy_3, friend_0, friend_1, friend_2, friend_3) VALUES (14, 14, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0)" >/dev/null
  sqlq "INSERT INTO game_faction_template (id, faction, faction_group, friend_group, enemy_group, enemy_0, enemy_1, enemy_2, enemy_3, friend_0, friend_1, friend_2, friend_3) VALUES (1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0)" >/dev/null
}
clear_hostility() {
  [ "$STAGED_HOSTILITY" != "1" ] && return 0
  sqlq "DELETE FROM game_faction_template WHERE id = 14" >/dev/null
  sqlq "DELETE FROM game_faction_template WHERE id = 1" >/dev/null
  STAGED_HOSTILITY=0
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
  # Re-arm the creature tick per scenario (2026-07-18): across a full ~40-test suite run the shared
  # node's combat processing degrades and combat-dependent tests intermittently see no mob swing /
  # no projectile impact (each PASSES standalone). A fresh tick schedule before each scenario is a
  # cheap robustness floor for the shared-state suite. See 270.
  scall debug_rearm_creature_tick || true
  GINGER=$(char_guid Ginger)
  [ -z "$GINGER" ] && timeout 60 "$WC" TEST test123 Ginger logout >/dev/null 2>&1 && GINGER=$(char_guid Ginger)
  [ -z "$GINGER" ] && { echo "[orch] no Ginger character" >&2; exit 1; }
  echo "[orch] $1: Ginger=$GINGER"
}
