#!/usr/bin/env bash
# Shared helpers for the test-scenario-*.sh orchestrators (work-item 140). Source, don't run.
# Convention: every orchestrator (1) seeds the idempotent scenario fixtures, (2) stages NPCs at a
# per-scenario pad via a short stay session, (3) runs the wire-client scenario mode, (4) sql-asserts
# server state at each seam, (5) tears down every spawned row and ASSERTS the teardown.

DB=lyracore
# THE CLIENT IS REACHED THROUGH THE ADAPTER, NEVER DIRECTLY (#244). `vanilla-wire` is a standalone
# build-5875 client with no default account, no default password and no knowledge of this project;
# adapters/lyracore/wire.sh is where this repo's fixture credentials, class default and endpoints
# live. Call it exactly like the old binary minus the password token:
#     "$WC" <account> <character> [scenario [args…]]
# Absolute (derived from this library's own location) so it works from any cwd, and overridable —
# point $WIRE_BIN at a released wire-harness build and the whole suite runs against that instead.
WC=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapters/lyracore/wire.sh

# $2/db is optional on every read/lookup helper below (default: the $DB global) — issue #70: a
# character living on a NON-default database (e.g. an instance or continent shard) used to read as
# "never went live" because every helper was hardwired to the single global. Pass a db explicitly
# only where a caller genuinely needs one shard's answer while $DB names another (see
# test-transfer-crash-matrix.sh's bring_home for the motivating case); every existing call site is
# unchanged because the parameter defaults to $DB when omitted.
sqlq() { spacetime sql "${2:-$DB}" "$1" 2>/dev/null; } # $1=query $2=database (optional)
scall() { spacetime call "$DB" -- "$@" >/dev/null 2>&1; }
# …the same call against a NAMED database. A character inside a routed instance (or on a continent
# shard) is not on $DB any more, and a debug reducer fired at $DB then fails with "no live entity"
# — silently, since scall swallows output. $1=database, rest = reducer + args.
scall_on() { local _db=$1; shift; spacetime call "$_db" -- "$@" >/dev/null 2>&1; }
# `${2:-}` not `$2`: callers that omit the database are the common case, and this library is sourced
# by scripts running under `set -u` (test-transfer-crash-matrix.sh does) where a bare `$2` on a
# one-argument call is a fatal unbound-variable error. Empty then falls through sqlq's own `${2:-$DB}`
# back to the global, so a caller passing nothing behaves exactly as it did before issue #70.
char_guid() { sqlq "SELECT guid FROM game_character WHERE name = '$1'" "${2:-}" | grep -oE '[0-9]+' | tail -1; } # $1=name $2=database (optional)
# first numeric column of the first data row
sql1() { sqlq "$1" "${2:-}" | sed -n 3p | awk -F'|' '{gsub(/ /,"",$1); print $1}'; } # $1=query $2=database (optional)

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
# through the REAL wire char-create path, echo the guid. Pair with drop_char in teardown.
fresh_char() { # $1=name  $2=class (warrior|...; default warrior)  $3=level (default 5)
  timeout 60 "$WC" TEST "$1" char-delete >/dev/null 2>&1 || true
  timeout 60 "$WC" TEST "$1" make-char "${2:-warrior}" >/dev/null 2>&1
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
  timeout 60 "$WC" TEST "$1" char-delete >/dev/null 2>&1 || true
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

# Drop $1 out of any party left behind by a crashed or failed prior run.
#
# Party state is REALM-CORE authoritative (#22/#50): the world shard's `game_group*` rows are a
# gateway-maintained MIRROR, so deleting them locally — which several tests did — leaves the real
# membership standing. The next invite then fails with `GroupFull` while $DB looks empty, and every
# party-dependent assertion after it fails for a reason that has nothing to do with what it tests.
# Uses the product's own LEAVE op (realm_op::LEAVE = 3) so the disband/mirror-sync rules run; the
# local delete is the single-database fallback. Both are best-effort — "not in a group" is success.
leave_any_group() { # $1=character guid
  spacetime call "${REALM_CORE_DB:-lyracore-realm}" -- realm_group_op 3 "$1" 0 0 0 >/dev/null 2>&1
  sqlq "DELETE FROM game_group_member WHERE character_guid = $1" >/dev/null 2>&1
  return 0
}

# Restart the gateway WITH ITS OWN ENVIRONMENT. $1 = log path (default /tmp/gw.log).
#
# A test that relaunches the gateway from a hardcoded command silently redefines the realm's
# topology for every test after it. test-playerbots.sh did exactly that — `LYRACORE_AOI=1` and nothing
# else, against a stack running four databases — and on a realm whose default shard has
# `hosts_instances = false` (correct for that topology) #48's guard REFUSED to start it. The gateway
# stayed down and the next twelve tests failed on "Connection refused", reported as twelve product
# regressions. Read the launch env off the live process instead, so a restart is a restart.
restart_gateway() {
  local log="${1:-/tmp/gw.log}" pid bin i
  pid=$(pgrep -x lyracore-gatewa | head -1)
  [ -n "$pid" ] || { echo "[lib] restart_gateway: no gateway running" >&2; return 1; }
  # `/proc/PID/exe` resolves to "<path> (deleted)" when the binary has been REBUILT under the
  # running process — which any `cargo build`/`cargo test` between the launch and here does. Strip
  # the marker and relaunch the path (the fresh binary at the same location is what we want); fall
  # back to argv[0] if that somehow does not exist.
  bin=$(readlink -f "/proc/$pid/exe" 2>/dev/null)
  bin=${bin% (deleted)}
  if [ ! -x "$bin" ]; then
    bin=$(tr '\0' '\n' < "/proc/$pid/cmdline" | head -1)
  fi
  [ -x "$bin" ] || { echo "[lib] restart_gateway: cannot locate the gateway binary ($bin)" >&2; return 1; }
  local envs=()
  while IFS= read -r line; do envs+=("$line"); done < <(tr '\0' '\n' < "/proc/$pid/environ" | grep -E '^(GW_[A-Z_]*|RUST_LOG)=')
  pkill -x lyracore-gatewa; sleep 1
  setsid nohup env "${envs[@]}" "$bin" </dev/null >"$log" 2>&1 &
  for i in $(seq 1 30); do
    grep -q "world listening" "$log" 2>/dev/null && { sleep 1; return 0; }
    sleep 1
  done
  echo "[lib] restart_gateway: no 'world listening' within 30s (log: $log)" >&2
  return 1
}

# Dissolve EVERY party in the realm. The suite's isolation step (267): party assertions count
# `game_group`/`game_group_member` GLOBALLY, so one surviving party from an earlier test makes the
# next one read "2 groups / 6 members" and fail for a reason that has nothing to do with it.
#
# They survive for a structural reason worth knowing: party state is authoritative on REALM-CORE
# (#22/#54), and a bot character is deleted on the WORLD SHARD — where the `character_owned!` sweep
# runs. A module cannot reach another database, so nothing sweeps the departed member's realm-core
# row, and the gateway's next mirror push writes the ghost party back onto the shard. Every bot
# party test therefore leaks one party, permanently. (In production that is a deleted character
# still listed in someone's party — worth an issue; here it is the suite's dominant flake source.)
#
# Drives the product's own ops: LEAVE each member on realm-core (disband happens on the last one),
# then push an EMPTY roster to each world shard's mirror, which is the documented disband case.
reset_party_state() {
  local rc="${REALM_CORE_DB:-lyracore-realm}" g
  for g in $(spacetime sql "$rc" "SELECT character_guid FROM game_group_member" 2>/dev/null | sed -n '3,$p' | grep -oE '[0-9]+'); do
    spacetime call "$rc" -- realm_group_op 3 "$g" 0 0 0 >/dev/null 2>&1
  done
  # `group_id`, not `id` (module/src/group.rs) — a wrong column name makes `spacetime sql` return an
  # ERROR that reads as "no rows" once stderr is swallowed, so the loop silently cleared nothing
  # (danger-zones §2, and it bit again here).
  for g in $(sqlq "SELECT group_id FROM game_group" | sed -n '3,$p' | grep -oE '[0-9]+'); do
    spacetime call "$DB" -- sync_group_mirror "$g" 0 0 2 0 '[]' >/dev/null 2>&1
  done
  return 0
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
  # `vanilla-wire` since #244 (the binary was renamed and the adapter wrapper exec's it, so the
  # backgrounded pid IS the client — see adapters/lyracore/wire.sh).
  if ! ps -o comm= -p "$1" 2>/dev/null | grep -q '^vanilla-wire$'; then
    echo "[orch] stay: pid $1 did not exit within ${2}s, but no longer looks like our wire-client" \
         "(stale/reused pid) — refusing to kill it" >&2
    return 0
  fi
  echo "[orch] stay: wire-client pid $1 did not exit within ${2}s — killing it" >&2
  kill -9 "$1" 2>/dev/null
  wait "$1" 2>/dev/null
}

SC_STAY_PID=""; SC_STAY_SENTINEL=""; SC_STAY_DEADLINE=""
stay_start() { # $1=account $2=char $3=deadline_secs (optional, default 60) $4=database (optional, default $DB)
  # The password argument is GONE (#244): the client takes no password on its command line, and
  # this account's fixture password lives in adapters/lyracore/wire.sh, which $WC points at.
  # deadline_secs (work-item 157): the wire client's stay mode self-exits (rc 0, silent) once its
  # own deadline elapses, regardless of whether stay_stop has been called yet. A caller that holds
  # the session live across a longer poll window than 60s (scenario-vendor's Step 4 fight-durability
  # poll runs up to 120s) MUST pass a deadline that exceeds that window, or the session — and the
  # character's live connection — can vanish mid-poll.
  # $5/database (issue #70): lets a caller materialise a session and poll for its entity on a
  # database OTHER than $DB (e.g. an instance/continent shard) without reassigning the global.
  local db="${4:-$DB}"
  local guid attempt; guid=$(char_guid "$2" "$db")
  local deadline="${3:-60}"
  SC_STAY_DEADLINE="$deadline"
  SC_STAY_SENTINEL="/tmp/sc_stay_$$_$2"
  # A fast relogin can race the PREVIOUS session's disconnect cleanup (which despawns the character
  # guid and would delete the NEW session's entity from under us) — settle first, then verify the
  # entity SURVIVES a beat after appearing, retrying the whole login once if it got reaped.
  sleep 2
  # Callers that want the wire session's output (the suite) set SC_STAY_LOG_DIR; default discards.
  local staylog="${SC_STAY_LOG_DIR:+${SC_STAY_LOG_DIR}/stay_$2.log}"
  for attempt in 1 2; do
    rm -f "$SC_STAY_SENTINEL"
    "$WC" "$1" "$2" stay "$SC_STAY_SENTINEL" "$deadline" >"${staylog:-/dev/null}" 2>&1 &
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
      echo "[orch] stay_start: $2 entity reaped by a stale disconnect (attempt $attempt) — retrying" >&2
    fi
    # Clear SC_STAY_PID as soon as this attempt's client is reaped (issue #70 review): if this was
    # the LAST attempt, stay_start returns 1 below with SC_STAY_PID now empty, so a caller that calls
    # stay_stop unconditionally after a failed stay_start (two such callers already exist — see
    # _stay_wait_exit's comment) finds nothing to wait on/kill, instead of re-using this attempt's
    # now-stale, already-reaped pid.
    touch "$SC_STAY_SENTINEL"; _stay_wait_exit "$SC_STAY_PID" $(( deadline + 15 )); SC_STAY_PID=""; sleep 2
  done
  echo "[orch] stay_start: $2 never went live" >&2
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
  stay_start TEST Ginger || return 1
  scall debug_teleport "$G" 0 -8968 -129 83.4 0 || { stay_stop; return 1; }
  stay_stop
  stay_start TEST2 dfsdfsd || return 1
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

# Every shard that can hold a durable game_character row (issue #213): realm-core holds
# accounts/parties only, never a character, so it is deliberately not in this list.
GINGER_SHARDS="lyracore lyracore-world-1 lyracore-world-2 lyracore-instances"

# Canonical stored position for the Ginger fixture — inside the map-0/region-1 box that
# `settle_home_shard` resolves onto lyracore (gateway/src/stdb/connection.rs's
# `region_db_for`). It is ALSO test-say-range.sh/test-move-relay.sh's `position_apart` target for
# Ginger, so parking her here after a repair leaves her exactly where those two already expect her.
GINGER_HOME_MAP=0; GINGER_HOME_X=-8968.0; GINGER_HOME_Y=-129.0; GINGER_HOME_Z=83.4

# Every character row named $1, one "guid db" pair per line, across every shard that can hold one.
_find_all_named() { # $1=name
  local db g
  for db in $GINGER_SHARDS; do
    g=$(sqlq "SELECT guid FROM game_character WHERE name = '$1'" "$db" | grep -oE '[0-9]+' | tail -1)
    [ -n "$g" ] && echo "$g $db"
  done
}

# Self-healing Ginger locator (issue #213: "no Ginger character" / "Ginger never went live"). The
# suite's shared fixture is assumed to live on lyracore by every hardcoded `DB=lyracore`
# in the standalone test-*.sh scripts, but she does not always stay there:
#   (a) map 0 (Eastern Kingdoms) is split into region 1 -> lyracore and region 2 ->
#       lyracore-world-2 (`spacetime sql lyracore-realm "SELECT * FROM game_region_assignment"`). A
#       login while her STORED position sits in region 2 (e.g. a prior test walked/teleported her
#       into Goldshire) makes `settle_home_shard` transfer her live row there — confirmed live via
#       the gateway log's `transfer 13: character 13 lyracore-world-2 -> lyracore` line when
#       walked back. Every char_guid/scall/sqlq call still hardwired to lyracore then finds
#       nothing: "no Ginger character".
#   (b) char-select CREATE always lands on the gateway's DEFAULT shard (issue #60) and is
#       resolved to a database only on the FIRST world login. A caller that could not find the real
#       Ginger and fell back to "log in to create her" (the exact fallback this function replaces)
#       mints a SECOND row also named Ginger. Nothing enforces one-name-per-account across shards,
#       and char-select's login-by-name is not guaranteed to pick the durable fixture over the
#       accidental duplicate — live-diagnosed as guid 3677 (stranded inside a Deadmines instance,
#       pending_instance_id set, level 1) shadowing guid 13 (the real fixture, level 5, on
#       lyracore-world-2): logins kept landing on 3677, so she "never went live" as far as any
#       overworld-shard poll was concerned.
# This scans every shard for $1, deletes every extra copy (keeping lyracore's copy outright if
# one exists, else the most-played survivor — cascades via debug_delete_character, same as any other
# fixture cleanup), then if the survivor is not on lyracore: logs in, teleports her (on her
# CURRENT shard) to the canonical home position, logs out, and logs back in once more so
# `settle_home_shard` transfers her row to lyracore. Echoes the final guid; returns 1 with a
# loud reason on stderr if the repair did not land her on lyracore.
ensure_ginger_home() { # $1=name (default Ginger)
  local name="${1:-Ginger}" hits guid db keep_guid="" keep_db="" best_secs=-1 cur_secs
  hits=$(_find_all_named "$name")
  if [ -z "$hits" ]; then
    # Never existed anywhere: create her via the real wire path. A fresh char spawns at the
    # Northshire start position (region 1), so the FIRST login settles her onto lyracore with
    # no repair needed below.
    timeout 60 "$WC" TEST "$name" logout >/dev/null 2>&1
    hits=$(_find_all_named "$name")
    [ -z "$hits" ] && { echo "[orch] ensure_ginger_home: could not create '$name' anywhere" >&2; return 1; }
  fi
  while read -r guid db; do
    [ -z "$guid" ] && continue
    cur_secs=$(sql1 "SELECT played_total_secs FROM game_character WHERE guid = $guid" "$db")
    cur_secs="${cur_secs:-0}"
    if [ "$db" = "lyracore" ] || { [ "$keep_db" != "lyracore" ] && [ "$cur_secs" -gt "$best_secs" ] 2>/dev/null; }; then
      keep_guid="$guid"; keep_db="$db"; best_secs="$cur_secs"
    fi
  done <<<"$hits"
  while read -r guid db; do
    if [ -z "$guid" ] || [ "$guid" = "$keep_guid" ]; then continue; fi
    echo "[orch] ensure_ginger_home: deleting stray duplicate '$name' guid=$guid on $db (keeping guid=$keep_guid on $keep_db)" >&2
    scall_on "$db" debug_delete_character "$guid"
  done <<<"$hits"
  if [ "$keep_db" = "lyracore" ]; then
    echo "$keep_guid"; return 0
  fi
  echo "[orch] ensure_ginger_home: '$name' (guid=$keep_guid) lives on $keep_db, not lyracore — walking her home" >&2
  stay_start TEST "$name" 30 "$keep_db" || { echo "[orch] ensure_ginger_home: could not log in on $keep_db" >&2; return 1; }
  scall_on "$keep_db" debug_teleport "$keep_guid" "$GINGER_HOME_MAP" "$GINGER_HOME_X" "$GINGER_HOME_Y" "$GINGER_HOME_Z" 0
  sleep 1
  stay_stop
  sleep 2
  stay_start TEST "$name" 10 lyracore || true
  stay_stop
  if [ "$(sqlq "SELECT guid FROM game_character WHERE name = '$name'" lyracore | grep -oE '[0-9]+' | tail -1)" = "$keep_guid" ]; then
    echo "$keep_guid"; return 0
  fi
  echo "[orch] ensure_ginger_home: '$name' (guid=$keep_guid) still not on lyracore after the repair attempt" >&2
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
  GINGER=$(ensure_ginger_home Ginger)
  [ -z "$GINGER" ] && { echo "[orch] no Ginger character" >&2; exit 1; }
  echo "[orch] $1: Ginger=$GINGER"
}
