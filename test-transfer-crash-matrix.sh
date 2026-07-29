#!/usr/bin/env bash
# test-transfer-crash-matrix.sh — issue #19 AC#3, made EXECUTABLE.
#
# AC#3 asks for `kill -9` at each of the seven cross-database transfer steps. Measured live, the
# whole seven-step drive commits in ~17ms:
#
#     20:00:32.707520  begin_transfer
#     20:00:32.721841  confirm_import
#     20:00:32.724152  finish_transfer
#
# No log-watcher-plus-pkill lands inside that window, let alone on a chosen step — so the procedure
# as written is not runnable, and a "pass" from random killing means nothing. This script uses the
# gateway's deterministic fault injector instead (`gateway/src/world/transfer.rs`):
#
#     GW_TRANSFER_ABORT_AFTER=<step>   # the named step COMMITS, then the process abort()s
#
# For each of the seven steps it: restarts the gateway with the injection + a two-database shard
# map, drives Ginger through the Deadmines portal (the live cross-database hop), waits for the
# injected death, asserts the crash-point invariant across BOTH databases, then restarts the gateway
# cleanly and asserts Ginger can get back in-world.
#
# THE INVARIANT THIS ASSERTS, AND WHY IT IS NOT "EXACTLY ONE ROW"
# --------------------------------------------------------------
# The escrow protocol DELIBERATELY has a window with two durable `game_character` copies: from
# `import_character_blob` (step 3) until `finish_transfer` (step 5) the source copy is frozen and the
# destination copy is fenced. That is the whole point of delete-last — a moment with two durable
# copies is safe, a moment with zero is unrecoverable. So the two invariants are:
#
#   ZERO-LOSS  at least one database holds a durable `game_character` row  (never zero)
#   NO-DUPE    at most one database holds a LIVE copy                      (never both)
#
# Neither of those is worth anything on its own, because Ginger's STAGED state — one durable,
# unfenced copy on the world database — satisfies both. So each step also asserts:
#
#   PROGRESS   after the injected death the SOURCE copy is fenced or gone   (the drive really ran)
#   SETTLE     after the clean restart + login, exactly one LIVE copy AND exactly one DURABLE copy
#              (one live but two durable = the player is fine and a copy is stranded — AC#3 asks
#              for whole on exactly ONE shard, and a stale ledger row wedges the guid's next hop)
#
# where LIVE = the row exists AND no `game_transfer_out`/`game_transfer_in` row fences it under this
# transfer id (transfer_id IS the character guid). This mirrors `FakeShardDb::live` in the gateway's
# headless crash matrix (`world::tests::a_gateway_kill_at_every_transfer_step_recovers_to_...`).
#
# STATUS: RUN, and green on both boundaries.
#   instance  (spacetime-core ↔ spacetime-instances, map 36) — 8/8, 2026-07-27, after #81's fix.
#   continent (spacetime-core ↔ spacetime-world-1,   map 1)  — #70; see that issue for the run.
# It reached 8/8 on the instance boundary only after #91/#98 (staging) and #81 (a real product
# defect this matrix is what found). Treat a change to it as a change to the instrument Phase B's
# crash guarantees rest on.
#
# TOPOLOGY: the gateway runs with `GW_REALM_CORE` (#100), so the character→shard index this exercises
# is realm-core's — the one production reads — rather than the default shard's own copy. The instance
# boundary therefore runs three databases and the continent boundary four.
#
# Prereqs (this script SKIPs loudly with rc 77 if they are missing):
#   - the destination database published with the debug feature and its operator claimed:
#       spacetime publish -s local -p module --build-options='--features=debug_reducers' <db>
#       spacetime call <db> claim_operator
#   - its world data loaded:  DB=<db> bash scripts/import-world.sh
#     (instance boundary needs map 36 on spacetime-instances; continent needs map 1 on
#      spacetime-world-1)
#
# NOTE: this test OWNS the gateway process for its duration — it kills and restarts it seven times
# and leaves it running with GW_SHARD_MAP set (same precedent as test-playerbots.sh's restart step).
# Run it when nobody is playing.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh

# ---------- which boundary (issue #70) ----------
# The crash points are the transfer drive's, not the boundary's, so the whole matrix is the same run
# either way — only the destination database, the destination map and HOW the crossing is driven
# differ. Phase A proved the instance boundary; #70 asks for the same guarantee re-earned on the
# continent one, which is a materially different hop: `dest_instance_id` is 0, so the two
# instance-only steps become no-ops whose crash points must still fire (asserted below).
BOUNDARY="${XCRASH_BOUNDARY:-instance}"
case "$BOUNDARY" in
  instance)
    IDB="${IDB:-spacetime-instances}"      # the instance shard (gateway/src/config.rs:63 convention)
    SHARD_MAP="${GW_SHARD_MAP:-36:*=spacetime-instances}"
    DEST_MAP=36
    # "Deadmines - Entering" → map 36. A real areatrigger, so this drives the crossing exactly as a
    # player does.
    DM_TRIGGER=78
    ;;
  continent)
    IDB="${IDB:-spacetime-world-1}"
    # THE `36:*` RULE STAYS, even though this boundary never touches map 36. Without it dungeon
    # instances resolve to `spacetime-core`, whose `game_config.hosts_instances` is FALSE, and the
    # gateway REFUSES TO START rather than degrade (issue #48's startup check — it fired on the
    # first continent run and is why this line is not just "1:*=..."). So this boundary runs on
    # THREE databases, which is also closer to production than the instance boundary's two.
    SHARD_MAP="${GW_SHARD_MAP:-36:*=spacetime-instances, 1:*=spacetime-world-1}"
    DEST_MAP=1
    # There is NO map-0 → map-1 areatrigger in the dump, and no boat/zeppelin/taxi anywhere in this
    # codebase (that is #69, and it is build rather than reuse). So the crossing is driven by a
    # cross-map `debug_teleport`, which reaches `world::teleport_player` — the SAME function the
    # areatrigger path ends in, and the same `MSG_MOVE_WORLDPORT_ACK` world entry drives the
    # transfer. This tests the transfer drive on the continent boundary, which is what #70 asks for;
    # it does NOT test a dock crossing, because none exists to test.
    DEST_X=-813.994; DEST_Y=-4920.57; DEST_Z=19.4341   # Durotar, inside the imported slice
    ;;
  *)
    echo "[xcrash] XCRASH_BOUNDARY='$BOUNDARY' is not 'instance' or 'continent'" >&2
    exit 2
    ;;
esac
REALM_CORE="${GW_REALM_CORE:-realm-core}"   # issue #100: the matrix runs the PRODUCTION topology
GWLOG=/tmp/gw_xcrash.log
PAD_X=-8930.0; PAD_Y=-250.0; PAD_Z=80.0    # the open-world staging pad (test-bot-deadmines.sh)
FAILED=0

# Derive the step list from the production source (`transfer::ABORT_STEPS`) instead of restating it
# (issue #70) — the hand-copied list here already drifted once (#34 added `publish_shard_index` to
# the drive and to `ABORT_STEPS` without touching this file, so the live matrix silently kept testing
# seven boundaries and reporting PASS). This is a plain-text extraction of the Rust array literal —
# no compiler, no `cargo run`, nothing that needs a node — so it costs nothing at run time and cannot
# itself drift: the list IS `ABORT_STEPS`, read straight off the file the headless matrix also uses.
#
# review (issue #70): the original form here (`sed -n '/pub const ABORT_STEPS/,/];/p'` with no line
# anchors, then `grep -oP '"\K[^"]+(?=")'` over the whole range) has two real failure modes, both
# reproduced against copies of this exact file:
#   1. A `//` comment ABOVE the declaration that happens to mention the constant's name and its own
#      "];" on the same source line (e.g. quoting an old array shape in a historical note) makes
#      sed's end-address close the range on THAT decoy line — the real array below is never read.
#   2. A `//` comment INSIDE the array block that contains a literal "];" substring (e.g. referencing
#      an old inline-call snippet) closes the range early too, silently truncating the extracted list
#      to whatever came before that comment (2 of 8 in one reproduction) — non-empty, so the old
#      zero-length guard below never fired, and the matrix would have reported "PASS — all 2 crash
#      points hold" while never touching the other 6.
# Fixed by: anchoring both sed addresses to the start of a line (so a mention inside a comment, which
# never starts a line with `pub const ABORT_STEPS` or consists ONLY of `];`, cannot match), dropping
# any line that is itself a comment before grepping for quoted strings (defeats a comment line that
# quotes a fake step name inline), and cross-checking the extracted count against the `[&str; N]`
# length the const's own line declares — Rust would refuse to compile a mismatch between that length
# and the literal, so it is a reliable independent witness against ANY malformed extraction (partial,
# padded, or duplicated), not just the two shapes reproduced above.
TRANSFER_RS=gateway/src/world/transfer.rs
DECLARED_N=$(grep -m1 -oP '^pub const ABORT_STEPS: \[&str; \K[0-9]+' "$TRANSFER_RS")
mapfile -t STEPS < <(sed -n '/^pub const ABORT_STEPS/,/^\];$/p' "$TRANSFER_RS" \
  | grep -v '^[[:space:]]*//' | grep -oP '"\K[^"]+(?=")')
if [ -z "$DECLARED_N" ] || [ "${#STEPS[@]}" -eq 0 ] || [ "${#STEPS[@]}" -ne "$DECLARED_N" ]; then
  # (the cross-check below runs against the FULL extraction — XCRASH_ONLY filters after it, so
  # narrowing the run can never satisfy the drift guard by accident)
  echo "[xcrash] could not extract ABORT_STEPS out of $TRANSFER_RS cleanly — declared length=" \
       "'${DECLARED_N:-?}' extracted=${#STEPS[@]}. The array literal's shape" >&2
  echo "[xcrash] changed (rename/reformat?); update the sed/grep extraction above to match it." >&2
  exit 2
fi
# DEBUGGING ONLY: narrow the run to one crash point. A full matrix is ~13 minutes, which is a long
# edit/observe cycle when you are chasing ONE step (#81 lives at `import_character_blob`). Never a
# pass: a narrowed run reports only the steps it ran, and the summary says so.
if [ -n "${XCRASH_ONLY:-}" ]; then
  mapfile -t STEPS < <(printf '%s\n' "${STEPS[@]}" | grep -Fx "$XCRASH_ONLY")
  [ "${#STEPS[@]}" -ge 1 ] || { echo "[xcrash] XCRASH_ONLY='$XCRASH_ONLY' names no step" >&2; exit 2; }
  echo "[xcrash] XCRASH_ONLY=$XCRASH_ONLY — NARROWED RUN, not an acceptance run"
fi

# ---------- cross-database SQL ----------
# scenario-lib's sql1 (unlike sqlq, see issue #70) has no database parameter, and this matrix's whole
# point is asking BOTH databases the same question — so this takes the database as an argument
# rather than growing sql1 a form nothing else needs. An unreadable count answers -1 rather than ""
# so a broken query fails an assertion loudly instead of reading as a zero.
db_count() { # $1=database $2=sql
  local v
  v=$(spacetime sql "$1" "$2" 2>/dev/null | sed -n 3p | awk -F'|' '{gsub(/ /,"",$1); print $1}')
  if [ -z "$v" ]; then
    echo "[xcrash] QUERY FAILED on database '$1': $2" >&2
    echo "-1"
  else
    echo "$v"
  fi
}
has_char() { db_count "$1" "SELECT COUNT(*) AS n FROM game_character WHERE guid = $2"; }
fenced_by() { # out-row (source claim) or in-row (arrival fence) under this transfer id
  local o i
  o=$(db_count "$1" "SELECT COUNT(*) AS n FROM game_transfer_out WHERE transfer_id = $2")
  i=$(db_count "$1" "SELECT COUNT(*) AS n FROM game_transfer_in WHERE transfer_id = $2")
  echo $(( o + i ))
}
live_on() { # 1 if the character is durable AND unfenced on $1, else 0
  local h f
  h=$(has_char "$1" "$2"); f=$(fenced_by "$1" "$2")
  if [ "$h" = "1" ] && [ "$f" = "0" ]; then echo 1; else echo 0; fi
}

# ---------- gateway lifecycle ----------
TOKEN=$(grep -oP 'spacetimedb_token = "\K[^"]+' ~/.config/spacetime/cli.toml || true)

# This test OWNS the gateway (see the header) and restarts it seven times with its OWN two-database
# topology + fault injector. Capture whatever was running BEFORE, and put it back on the way out —
# otherwise every suite run silently leaves the realm on the matrix's topology and binary, and the
# next thing anyone measures (a bench, a manual login, the next suite run's first test) is measuring
# a configuration nobody chose. Same failure the playerbots restart caused mid-suite, just at the end.
_ORIG_PID=$(pgrep -x gateway | head -1)
if [ -n "${_ORIG_PID:-}" ]; then
  _ORIG_BIN=$(readlink -f "/proc/$_ORIG_PID/exe" 2>/dev/null); _ORIG_BIN=${_ORIG_BIN% (deleted)}
  mapfile -t _ORIG_ENV < <(tr '\0' '\n' < "/proc/$_ORIG_PID/environ" 2>/dev/null | grep -E '^(GW_[A-Z_]*|RUST_LOG)=')
fi
restore_original_gateway() {
  [ -n "${_ORIG_BIN:-}" ] && [ -x "${_ORIG_BIN:-}" ] || return 0
  pkill -x gateway 2>/dev/null; sleep 1
  setsid nohup env "${_ORIG_ENV[@]}" "$_ORIG_BIN" </dev/null >/tmp/gw_restored.log 2>&1 &
  local i
  for i in $(seq 1 25); do
    grep -q 'world listening' /tmp/gw_restored.log 2>/dev/null && { echo "[xcrash] restored the pre-matrix gateway ($_ORIG_BIN)"; return 0; }
    sleep 1
  done
  echo "[xcrash] WARNING: could not restore the pre-matrix gateway — the realm is still on the matrix's topology" >&2
}
trap restore_original_gateway EXIT
gw_start() { # $1 = step to abort after ("" = a clean gateway)
  pkill -x gateway 2>/dev/null   # -x, never -f: `-f` self-matches the launching shell (danger-zones §3)
  sleep 1
  : >"$GWLOG"
  # Bash < 4.4 errors on "${arr[@]}" for an empty array under `set -u`; the ${a[@]+...} guard is the
  # portable way to say "expand only if non-empty".
  local -a inject=()
  [ -n "${1:-}" ] && inject=(GW_TRANSFER_ABORT_AFTER="$1")
  # GW_REALM_CORE IS PART OF THE PRODUCTION SHAPE (issue #100). Without it `realm_core()` falls back
  # to the DEFAULT shard, so the character→shard index every routing decision consults is
  # spacetime-core's own copy rather than realm-core's — i.e. the matrix was validating a topology
  # the server does not run in, and #81 (a routing defect found BY this matrix) lived in exactly that
  # code. Also makes `publish_shard_index`, a step the matrix injects a crash after, write to a real
  # target instead of a no-op.
  setsid nohup env GW_AOI=1 GW_SHARD_MAP="$SHARD_MAP" GW_REALM_CORE="$REALM_CORE" \
    GW_COORDINATOR_TOKEN="$TOKEN" \
    RUST_LOG=info,gateway::world=debug ${inject[@]+"${inject[@]}"} \
    ./target/debug/gateway </dev/null >"$GWLOG" 2>&1 &
  local i
  for i in $(seq 1 25); do
    grep -q 'world listening' "$GWLOG" && { sleep 2; return 0; }
    sleep 1
  done
  echo "[xcrash] gateway never printed 'world listening' within 25s (log: $GWLOG)" >&2
  tail -5 "$GWLOG" >&2
  return 1
}
gw_dead() { ! pgrep -x gateway >/dev/null 2>&1; }
wait_for_gw_death() { # $1=secs
  local i
  for i in $(seq 1 "$1"); do gw_dead && return 0; sleep 1; done
  return 1
}

# Put Ginger back on the world database, in the open world, on the staging pad — whichever shard she
# is currently on. Needs a CLEAN gateway running: the cross-database hop home is driven by a login,
# exactly like the outbound one.
bring_home() {
  local guid=$1 holder=$DB
  [ "$(has_char "$DB" "$guid")" = "1" ] || holder=$IDB

  # NORMALISE THE TWO-COPY START STATE (issue #81's leftovers). #81 settles the player but strands a
  # durable copy on the other database under an escrow row the cross-database reaper correctly HOLDs
  # forever. bring_home used to look only at '$DB' — one copy there satisfied it — so the run AFTER a
  # #81 failure aborted all eight steps at staging and reported eight FAILs that measured nothing.
  # The matrix could not recover from the very defect it exists to detect.
  #
  # This normalises; it does not hide. Every step's post-recovery assertion re-detects #81 from a
  # clean start, which is the only place the finding is worth anything. Both clears are announced.
  #
  # Escrow first, then the copy: a fenced character REFUSES both login (the in-transit chokepoint)
  # and `debug_delete_character` (CHAR_IN_TRANSIT), so a stale ledger row blocks its own cleanup.
  # Raw SQL because no reducer clears a lone out-row by design — `finish_transfer` refuses without
  # the arrival attestation and `release_transfer` refuses when an out-row exists, which is correct
  # for the protocol and leaves the harness to wipe its own fixture (same precedent as the
  # `game_world_entity`/`game_instance_binding` deletes below).
  local _e
  for _db in "$DB" "$IDB"; do
    for _t in game_transfer_out game_transfer_in; do
      _e=$(db_count "$_db" "SELECT COUNT(*) AS n FROM $_t WHERE transfer_id = $guid")
      if [ "$_e" != "0" ] && [ "$_e" != "-1" ]; then
        echo "[xcrash] staging: clearing $_e stale $_t row(s) for $guid on '$_db' (left by an earlier run)"
        spacetime sql "$_db" "DELETE FROM $_t WHERE transfer_id = $guid" >/dev/null 2>&1
      fi
    done
  done
  # Only ever drop the copy on the shard that is NOT home, and only when home still holds one — so a
  # bug here can never destroy the last durable copy of the fixture. `$guid` is always Ginger's,
  # resolved by name on '$DB' and checked non-empty before the matrix starts, so this can never name
  # another character — which matters now the destination can be `spacetime-world-1`, a database
  # holding a real continent and a second fixture (Kaltest, guid 12).
  #
  # An UNREADABLE destination refuses rather than deletes: `db_count` answers -1 on a failed query,
  # and -1 is `!= 0`, so the obvious form of this test would cascade-delete on a state it could not
  # actually read. `has_char` returning -1 is exactly the shape hazard 7 keeps producing (a wrong
  # column name reads as a failure whose stderr is swallowed).
  _idb_copies=$(has_char "$IDB" "$guid")
  if [ "$_idb_copies" = "-1" ]; then
    echo "[xcrash] bring_home: could not read '$IDB' for guid $guid — refusing to stage rather than" \
         "acting on a state I cannot read" >&2
    return 1
  fi
  if [ "$holder" = "$DB" ] && [ "$_idb_copies" != "0" ]; then
    echo "[xcrash] staging: '$IDB' holds a STRANDED durable copy of $guid (issue #81's failure state)" \
         "— cascade-deleting it so this run starts single-copy"
    spacetime call "$IDB" -- debug_delete_character "$guid" >/dev/null 2>&1
    if [ "$(has_char "$IDB" "$guid")" != "0" ]; then
      echo "[xcrash] bring_home: could not clear the stranded copy on '$IDB' — refusing to run" >&2
      return 1
    fi
  fi
  if [ "$holder" = "$IDB" ]; then
    # Cross-map teleport writes the DESTINATION into the character row and despawns the entity; the
    # next world entry is what actually drives the transfer back (`settle_transfer`).
    #
    # ORDER IS LOAD-BEARING (first live run, 2026-07-25): `debug_teleport` resolves the mover through
    # `entity_by_owner` and REFUSES with "no live entity for guid N" when the character is durable but
    # logged out — which is exactly the state a crash-recovered Ginger is in on the instance shard. The
    # original order (teleport, then log in) therefore no-oped every time, and because the call's error
    # was swallowed the matrix reported "could not stage" with no reason. Materialise her FIRST with a
    # held session, teleport while she is live, then drop it.
    # scenario-lib's stay_start now takes the database as its $5 parameter (issue #70) instead of
    # requiring callers to reassign the global $DB around the call — it would otherwise poll
    # `game_world_entity` on the world shard for a character who is live on the INSTANCE shard, and
    # time out with "never went live".
    if ! stay_start TEST test123 Ginger 60 "$IDB"; then
      echo "[xcrash] bring_home: could not open a session on '$IDB' to materialise Ginger" >&2
      return 1
    fi
    local tp_err
    if ! tp_err=$(spacetime call "$IDB" -- debug_teleport "$guid" 0 $PAD_X $PAD_Y $PAD_Z 0 2>&1); then
      echo "[xcrash] bring_home: debug_teleport on '$IDB' failed: $tp_err" >&2
      stay_stop
      return 1
    fi
    stay_stop
    # The world entry on the NEXT login is what drives `settle_transfer` back to '$DB'.
    timeout 90 "$WC" TEST test123 Ginger logout >/dev/null 2>&1
  fi
  if [ "$(has_char "$DB" "$guid")" != "1" ]; then
    echo "[xcrash] could not bring Ginger home to '$DB' — aborting the matrix rather than reporting" \
         "results against an unknown starting state" >&2
    return 1
  fi
  sqlq "DELETE FROM game_instance_binding WHERE character_guid = $guid" >/dev/null
  # `game_character` has NO `instance_id` column — it is `pending_instance_id` (module/src/character.rs).
  # Naming a column that does not exist makes spacetime reject the WHOLE statement, and `sqlq`
  # swallows the error (2>/dev/null, rc unchecked): the staging then silently does nothing and the
  # matrix runs against an unknown starting position. test-bot-deadmines.sh is the precedent for the
  # same pad, and it stages no instance column at all.
  sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z, map_id = 0, pending_instance_id = 0 WHERE guid = $guid" >/dev/null
  # Clear the STALE LIVE ENTITY, on every database (issue #91). This is the one that actually decides
  # routing: `Coordinator::character_location` (gateway/src/stdb/reads.rs) prefers `game_world_entity`
  # over the durable row —
  #     if let Some(e) = db.game_world_entity().guid().find(&guid) { return Some((e.map_id, e.instance_id)); }
  # — and a step whose INJECTED ABORT killed the gateway mid-session leaves that row behind, still
  # carrying the instance location. Staging the durable row to map 0 while that row survives means the
  # next login resolves to the OLD location, drives a cross-database transfer on world entry, and the
  # armed injector fires on it — killing the gateway before the test has driven its own portal. Every
  # step then failed at staging with "never went live", and the eight FAILs said nothing about the
  # crash points they were supposed to exercise. Both databases, because the stale row can be on
  # either side of the boundary depending on where the abort landed.
  for _db in "$DB" "$IDB"; do
    spacetime sql "$_db" "DELETE FROM game_world_entity WHERE guid = $guid" >/dev/null 2>&1
  done
  # ASSERT the write landed. A silently-rejected UPDATE is exactly how the bug above hid.
  if [ "$(db_count "$DB" "SELECT COUNT(*) AS n FROM game_character WHERE guid = $guid AND map_id = 0")" != "1" ]; then
    echo "[xcrash] the staging UPDATE did not take — Ginger is not on map 0 on '$DB' (rejected SQL?)" >&2
    return 1
  fi
  # ASSERT WHAT ROUTING WILL ANSWER, not merely what we just wrote (issue #91). The pre-#91 assertion
  # checked `game_character.map_id = 0` — the very row the UPDATE above had just set — so it passed
  # against a character that was still going to transfer on its next world entry. Assert the same
  # thing `character_location` reads: no live entity anywhere, on any database, for this guid.
  local _stale
  for _db in "$DB" "$IDB"; do
    _stale=$(db_count "$_db" "SELECT COUNT(*) AS n FROM game_world_entity WHERE guid = $guid")
    if [ "$_stale" != "0" ]; then
      echo "[xcrash] staging left a live game_world_entity row for $guid on '$_db' ($_stale) —" \
           "character_location prefers it over the durable row, so the next login would transfer" \
           "instead of testing the crash point (issue #91)" >&2
      return 1
    fi
  done
}

# ---------- preflight ----------
cargo build -q -p wire-client || { echo "[xcrash] wire-client build failed" >&2; exit 2; }
cargo build -q -p spacetime-core-gateway || { echo "[xcrash] gateway build failed" >&2; exit 2; }
[ -n "$TOKEN" ] || { echo "[xcrash] no spacetimedb_token in ~/.config/spacetime/cli.toml" >&2; exit 2; }

if ! spacetime sql "$IDB" "SELECT COUNT(*) AS n FROM game_character" >/dev/null 2>&1; then
  echo "SKIP: second database '$IDB' is not published/readable — AC#3 is a CROSS-DATABASE matrix and"
  echo "SKIP: cannot run on a single-database node. See this script's header for the two publish +"
  echo "SKIP: claim_operator + import lines that provision it."
  exit 77
fi
if [ "$(db_count "$IDB" "SELECT COUNT(*) AS n FROM game_creature_spawn WHERE map_id = $DEST_MAP")" -lt 1 ]; then
  echo "SKIP: '$IDB' holds no map-$DEST_MAP spawns — run: DB=$IDB bash scripts/import-world.sh"
  exit 77
fi

gw_start "" || exit 2
GINGER=$(char_guid Ginger)
if [ -z "$GINGER" ]; then
  timeout 60 "$WC" TEST test123 Ginger logout >/dev/null 2>&1
  GINGER=$(char_guid Ginger)
fi
[ -n "$GINGER" ] || { echo "[xcrash] no Ginger character on '$DB'" >&2; exit 2; }
echo "[xcrash] boundary=$BOUNDARY  Ginger=$GINGER  world='$DB'  destination='$IDB' (map $DEST_MAP)  shard-map='$SHARD_MAP'"

# ---------- the matrix ----------
declare -a MATRIX=()
for STEP in "${STEPS[@]}"; do
  echo
  echo "──────── GW_TRANSFER_ABORT_AFTER=$STEP ────────"
  STEP_FAILED=0
  note() { echo "[xcrash][$STEP] $*"; }
  bad()  { echo "[xcrash][$STEP] ASSERT FAIL: $*" >&2; STEP_FAILED=1; FAILED=1; }
  good() { echo "[xcrash][$STEP] ASSERT OK: $*"; }

  # 1. clean gateway → known starting state on the world database.
  gw_start "" || { bad "clean gateway would not start"; MATRIX+=("HARNESS $STEP"); continue; }
  bring_home "$GINGER" || { bad "could not stage Ginger on '$DB'"; MATRIX+=("HARNESS $STEP"); continue; }
  spacetime call "$IDB" -- release_transfer "$GINGER" >/dev/null 2>&1 # clear any fence a previous step left
  # ...and PROVE the instance shard is empty of her before arming anything. `bring_home` only
  # guarantees a copy on '$DB'; it never looks at '$IDB'. A copy left there by an earlier step makes
  # ZERO-LOSS pass no matter what the injection does (the count is already >= 1 before the transfer
  # starts), and the `release_transfer` above has just UN-FENCED it — so the pair would be live on
  # both shards before this step's crash point is even reached. Refuse to report against that.
  STAGED_I=$(has_char "$IDB" "$GINGER")
  if [ "$STAGED_I" != "0" ]; then
    bad "staging: '$IDB' still holds $STAGED_I durable copy/copies of Ginger BEFORE the injection — every assertion below would be measured against a two-copy start state. Aborting this step rather than reporting against it."
    # HARNESS, not FAIL: the injection was never armed, so this step says NOTHING about its crash
    # point (issue #91's rule, applied to the one abort path that still called itself a FAIL — which
    # is how a run that measured nothing at all reported eight product failures).
    MATRIX+=("HARNESS $STEP"); continue
  fi
  note "staged: Ginger on '$DB' ONLY, map 0, pad ($PAD_X $PAD_Y $PAD_Z)"

  # 2. restart WITH the injection armed.
  gw_start "$STEP" || { bad "injected gateway would not start"; MATRIX+=("HARNESS $STEP"); continue; }

  # 3. drive the portal. A HELD session is required: the transfer runs inside the client's loading
  #    screen (the WORLDPORT_ACK handler), and `stay` drains with the decoding recv() that answers
  #    SMSG_NEW_WORLD. Deadline well past the whole window — `stay` exits rc 0 on timeout, silently.
  if ! stay_start TEST test123 Ginger 180; then
    bad "Ginger never went live before the portal — cannot attribute what follows to the injection"
    pkill -x wire-client 2>/dev/null; MATRIX+=("HARNESS $STEP"); continue
  fi
  sleep 2
  if [ "$BOUNDARY" = instance ]; then
    scall debug_enter_areatrigger "$GINGER" $DM_TRIGGER
  else
    # `debug_teleport` REFUSES without a live entity ("no live entity for guid N"), which is why the
    # session above is held rather than dropped first — the same ordering `bring_home` learned the
    # hard way on 2026-07-25.
    scall debug_teleport "$GINGER" $DEST_MAP $DEST_X $DEST_Y $DEST_Z 0
  fi

  # 4. the injected death. This IS the observation that the crash point was reached.
  if wait_for_gw_death 45; then
    if grep -q 'ABORTING BY FAULT INJECTION' "$GWLOG"; then
      good "gateway aborted by injection after $STEP"
    else
      bad "gateway died within 45s but logged no injection line — it crashed for some OTHER reason, so nothing below is attributable to $STEP (log: $GWLOG)"
      tail -8 "$GWLOG" >&2
    fi
  else
    bad "gateway still alive 45s after the portal fired — GW_TRANSFER_ABORT_AFTER=$STEP never triggered (typo'd step name? transfer never routed cross-database? check $GWLOG for 'names no transfer step')"
    grep -E 'transfer |names no transfer step' "$GWLOG" | tail -5 >&2
  fi
  stay_stop
  pkill -x wire-client 2>/dev/null

  # 4b. AC#2 (#70): a continent hop carries `dest_instance_id == 0`, so `ensure_instance` and
  # `evict_instance_population` do no work. Their `abort_point`s sit OUTSIDE the
  # `if escrow.dest_instance_id != 0` guards in `run_transfer_injected` (deliberately — see the
  # comment there), so the crash points must still FIRE. The injection assertion above is what
  # proves they fired; this is what proves the steps were no-ops rather than quietly mirroring an
  # instance onto a continent shard. Asserted on every step, not just those two: no step of a
  # continent hop may create one.
  if [ "$BOUNDARY" = continent ]; then
    _inst=$(db_count "$IDB" "SELECT COUNT(*) AS n FROM game_instance")
    _bind=$(db_count "$IDB" "SELECT COUNT(*) AS n FROM game_instance_binding WHERE character_guid = $GINGER")
    if [ "$_inst" = "0" ] && [ "$_bind" = "0" ]; then
      good "instance-only steps were NO-OPS: '$IDB' mirrored no instance and bound none for $GINGER"
    else
      bad "a continent hop mirrored an instance on '$IDB' (game_instance=$_inst, bindings for $GINGER=$_bind) — dest_instance_id must be 0 on this boundary, so both instance steps should have done nothing"
    fi
  fi

  # 5. THE CRASH-POINT INVARIANT, read straight off both databases.
  HW=$(has_char "$DB" "$GINGER");  HI=$(has_char "$IDB" "$GINGER")
  LW=$(live_on  "$DB" "$GINGER");  LI=$(live_on  "$IDB" "$GINGER")
  note "durable: $DB=$HW $IDB=$HI   live(unfenced): $DB=$LW $IDB=$LI"
  # PROGRESS — the one assertion that is not satisfied by "nothing happened". Ginger sits durable
  # and unfenced on '$DB' at the start of every step, and in THAT state ZERO-LOSS (1 >= 1), NO-DUPE
  # (1 <= 1) and the post-recovery check (1 live) all pass. The injected death above is the only
  # other non-vacuous check, so assert the DURABLE evidence too: every crash point from step 1 on
  # leaves the source copy either FENCED (1-4: its out-row, or the confirm in-row) or GONE (5-7,
  # finish_transfer deleted it). Still durable AND unfenced means the drive never reached step 1 —
  # the portal never fired, or the transfer never routed cross-database — and nothing below is
  # attributable to $STEP. Robust to the reaper landing first: roll-forward leaves it GONE.
  FW=$(fenced_by "$DB" "$GINGER")
  if [ "$HW" = "0" ]; then
    good "PROGRESS: the source copy is GONE from '$DB' — the drive reached $STEP (>= finish_transfer)"
  elif [ "${FW:-0}" -ge 1 ] 2>/dev/null; then
    good "PROGRESS: the source copy on '$DB' is FENCED by $FW escrow ledger row(s) — the drive really reached $STEP"
  else
    bad "PROGRESS: Ginger is still durable AND UNFENCED on '$DB' (durable=$HW, fences=$FW) — the transfer never got past begin_transfer, so ZERO-LOSS/NO-DUPE/recovery below would all pass VACUOUSLY. Nothing here is attributable to $STEP."
  fi
  if [ $(( HW + HI )) -ge 1 ]; then
    good "ZERO-LOSS: $(( HW + HI )) durable game_character copy/copies survive the crash"
  else
    bad "ZERO-LOSS violated: expected >=1 durable game_character row for guid $GINGER, found 0 ($DB=$HW, $IDB=$HI) — THE CHARACTER WAS LOST"
  fi
  if [ $(( LW + LI )) -le 1 ]; then
    good "NO-DUPE: $(( LW + LI )) live (unfenced) copy/copies"
  else
    bad "NO-DUPE violated: expected <=1 LIVE copy, found $(( LW + LI )) (live on $DB=$LW, live on $IDB=$LI) — the character is playable on BOTH shards"
  fi

  # 6. recovery: a clean gateway, and Ginger must get back in-world. Whether the transfer is
  #    re-driven forward or rolled back is the protocol's business; a character that cannot log in
  #    after a restart is a FAILURE either way.
  gw_start "" || { bad "clean gateway would not restart after the injected crash"; MATRIX+=("HARNESS $STEP"); continue; }
  if timeout 90 "$WC" TEST test123 Ginger logout >/tmp/xcrash_reentry_${BOUNDARY}_$STEP.log 2>&1; then
    good "re-entry: a fresh wire session logged Ginger back into the world after the crash"
  elif grep -q 'M1 OK — in world' "/tmp/xcrash_reentry_${BOUNDARY}_$STEP.log"; then
    # The assertion is "can she get back IN", and the wire client prints M1 OK the moment she is in
    # world — the `logout` verb is just how this probe ends. Recovery lands her in Deadmines, where a
    # Defias can put her in combat before the probe gets to CMSG_LOGOUT_REQUEST, and the server then
    # correctly answers FailureInCombat. Failing the step on that reports the game working as a crash
    # defect (it did once, on evict_instance_population). Assert the EFFECT, not the exit code.
    good "re-entry: Ginger reached the world; the logout probe then failed ($(grep -o 'got [A-Za-z]*' "/tmp/xcrash_reentry_${BOUNDARY}_$STEP.log" | tail -1)) — not a re-entry failure"
  else
    bad "re-entry: Ginger could NOT re-enter the world after a clean gateway restart (in-transit forever?) — see /tmp/xcrash_reentry_${BOUNDARY}_$STEP.log"
    tail -5 "/tmp/xcrash_reentry_${BOUNDARY}_$STEP.log" >&2
  fi
  # PRESERVE THE RECOVERY LOG. `gw_start` truncates $GWLOG, so the run that matters — the clean
  # gateway re-driving (or failing to re-drive) the abandoned transfer — is erased by the NEXT step
  # before anyone can read it. That is why #81 went three rounds on inference: the one artefact that
  # distinguishes "the resume was never driven" from "it was driven and failed" was thrown away.
  cp "$GWLOG" "/tmp/xcrash_recovery_${BOUNDARY}_$STEP.log" 2>/dev/null
  HW=$(has_char "$DB" "$GINGER"); HI=$(has_char "$IDB" "$GINGER")
  LW=$(live_on "$DB" "$GINGER");  LI=$(live_on "$IDB" "$GINGER")
  if [ $(( LW + LI )) -eq 1 ]; then
    good "post-recovery: settled LIVE on exactly one database ($DB=$LW, $IDB=$LI)"
  else
    bad "post-recovery: expected exactly 1 LIVE copy after the restart+login, found $(( LW + LI )) (durable $DB=$HW/$IDB=$HI, live $DB=$LW/$IDB=$LI) — recovery did not settle"
  fi
  # AC#3 says WHOLE ON EXACTLY ONE SHARD, and the live count alone does not say that: a recovery
  # that puts the player back in the world while leaving a stranded durable copy behind (frozen
  # under a never-cleared out-row, or orphaned unfenced) reads as 1 LIVE and would be reported as a
  # PASS. Two durable copies are the safe state only DURING the protocol; once it has settled they
  # are a leak, and the frozen one can wedge the guid's next transfer (`BeginPlan::Replay` on a
  # stale ledger row — see `settle_transfer`'s no-escrow arm).
  if [ $(( HW + HI )) -eq 1 ]; then
    good "post-recovery: exactly one DURABLE copy remains ($DB=$HW, $IDB=$HI)"
  else
    bad "post-recovery: expected exactly 1 DURABLE game_character row after the restart+login, found $(( HW + HI )) ($DB=$HW, $IDB=$HI) — the transfer settled the PLAYER but stranded a copy on the other database"
  fi

  if [ "$STEP_FAILED" -eq 0 ]; then
    echo "[xcrash][$STEP] PASS"; MATRIX+=("PASS $STEP")
  else
    echo "[xcrash][$STEP] FAIL"; MATRIX+=("FAIL $STEP")
  fi
done

# ---------- teardown: clean gateway, Ginger home, no instance left ticking ----------
echo
gw_start "" || FAILED=1
bring_home "$GINGER" || FAILED=1
pkill -x wire-client 2>/dev/null

echo
echo "════════ transfer crash matrix — $BOUNDARY boundary ($DB ↔ $IDB) ════════"
for r in "${MATRIX[@]}"; do printf "  %s\n" "$r"; done
echo "───────────────────────────────────────"
# A HARNESS row means the step never REACHED its crash point (staging failed, the gateway would not
# start, the character never went live) — a different claim from "the crash point did not hold", and
# reporting them identically is how eight meaningless FAILs once read as a result (issue #91). Never
# claim a pass while any step was skipped: an unreached crash point is unknown, not green.
_harness=0
for r in "${MATRIX[@]}"; do case "$r" in HARNESS\ *) _harness=$((_harness+1));; esac; done
if [ "$_harness" -ne 0 ]; then
  echo "[xcrash] HARNESS BROKEN — $_harness of ${#STEPS[@]} step(s) never reached their crash point."
  echo "[xcrash] Those steps are UNKNOWN, not failed. Fix the harness and re-run before reading"
  echo "[xcrash] anything into the rest of this table."
  exit 2
fi
if [ "$FAILED" -eq 0 ]; then
  echo "[xcrash] PASS — all ${#STEPS[@]} crash points hold ZERO-LOSS + NO-DUPE and recover"
  exit 0
fi
echo "[xcrash] FAIL"
exit 1
