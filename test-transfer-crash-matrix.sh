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
# where LIVE = the row exists AND no `game_transfer_out`/`game_transfer_in` row fences it under this
# transfer id (transfer_id IS the character guid). This mirrors `FakeShardDb::live` in the gateway's
# headless crash matrix (`world::tests::a_gateway_kill_at_every_transfer_step_recovers_to_...`).
#
# STATUS: **UNRUN.** Written against a live two-client session that could not be disturbed; every
# assertion below is unexercised. Treat the first run as the real acceptance run.
#
# Prereqs (this script SKIPs loudly with rc 77 if they are missing):
#   - the second database published with the debug feature and its operator claimed:
#       spacetime publish -s local -p module --build-options='--features=debug_reducers' spacetime-instances
#       spacetime call spacetime-instances claim_operator
#   - map 36 world data loaded into it:  DB=spacetime-instances bash scripts/import-world.sh
#
# NOTE: this test OWNS the gateway process for its duration — it kills and restarts it seven times
# and leaves it running with GW_SHARD_MAP set (same precedent as test-playerbots.sh's restart step).
# Run it when nobody is playing.
set -uo pipefail
cd "$(dirname "$0")/../.."
source tools/wire-client/scenario-lib.sh

IDB="${IDB:-spacetime-instances}"          # the instance shard (gateway/src/config.rs:63 convention)
SHARD_MAP="${GW_SHARD_MAP:-36:*=spacetime-instances}"
GWLOG=/tmp/gw_xcrash.log
PAD_X=-8930.0; PAD_Y=-250.0; PAD_Z=80.0    # the open-world staging pad (test-bot-deadmines.sh)
DM_TRIGGER=78                              # "Deadmines - Entering" → map 36
FAILED=0

# ponytail: the seven step names are a literal list of strings — the same list as
# `transfer::ABORT_STEPS` and as the headless matrix's `kill_at` loop. If that list ever grows, this
# one is a one-line edit; a shared generator would cost more than it saves.
STEPS=(
  begin_transfer
  ensure_instance
  import_character_blob
  confirm_import
  finish_transfer
  release_transfer
  evict_instance_population
)

# ---------- cross-database SQL ----------
# scenario-lib's sqlq/sql1 are hardwired to $DB; the whole point here is asking BOTH databases the
# same question, so this takes the database as an argument. An unreadable count answers -1 rather
# than "" so a broken query fails an assertion loudly instead of reading as a zero.
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
gw_start() { # $1 = step to abort after ("" = a clean gateway)
  pkill -x gateway 2>/dev/null   # -x, never -f: `-f` self-matches the launching shell (danger-zones §3)
  sleep 1
  : >"$GWLOG"
  # Bash < 4.4 errors on "${arr[@]}" for an empty array under `set -u`; the ${a[@]+...} guard is the
  # portable way to say "expand only if non-empty".
  local -a inject=()
  [ -n "${1:-}" ] && inject=(GW_TRANSFER_ABORT_AFTER="$1")
  setsid nohup env GW_AOI=1 GW_SHARD_MAP="$SHARD_MAP" GW_COORDINATOR_TOKEN="$TOKEN" \
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
  if [ "$holder" = "$IDB" ]; then
    # Cross-map teleport writes the DESTINATION into the character row and despawns the entity; the
    # next world entry is what actually drives the transfer back (`settle_transfer`).
    spacetime call "$IDB" -- debug_teleport "$guid" 0 $PAD_X $PAD_Y $PAD_Z 0 >/dev/null 2>&1
    timeout 90 "$WC" TEST test123 Ginger logout >/dev/null 2>&1
  fi
  if [ "$(has_char "$DB" "$guid")" != "1" ]; then
    echo "[xcrash] could not bring Ginger home to '$DB' — aborting the matrix rather than reporting" \
         "results against an unknown starting state" >&2
    return 1
  fi
  sqlq "DELETE FROM game_instance_binding WHERE character_guid = $guid" >/dev/null
  sqlq "UPDATE game_character SET x = $PAD_X, y = $PAD_Y, z = $PAD_Z, map_id = 0, instance_id = 0 WHERE guid = $guid" >/dev/null
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
if [ "$(db_count "$IDB" "SELECT COUNT(*) AS n FROM game_creature_spawn WHERE map_id = 36")" -lt 1 ]; then
  echo "SKIP: '$IDB' holds no map-36 spawns — run: DB=$IDB bash scripts/import-world.sh"
  exit 77
fi

gw_start "" || exit 2
GINGER=$(char_guid Ginger)
if [ -z "$GINGER" ]; then
  timeout 60 "$WC" TEST test123 Ginger logout >/dev/null 2>&1
  GINGER=$(char_guid Ginger)
fi
[ -n "$GINGER" ] || { echo "[xcrash] no Ginger character on '$DB'" >&2; exit 2; }
echo "[xcrash] Ginger=$GINGER  world='$DB'  instances='$IDB'  shard-map='$SHARD_MAP'"

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
  gw_start "" || { bad "clean gateway would not start"; MATRIX+=("FAIL $STEP"); continue; }
  bring_home "$GINGER" || { bad "could not stage Ginger on '$DB'"; MATRIX+=("FAIL $STEP"); continue; }
  spacetime call "$IDB" -- release_transfer "$GINGER" >/dev/null 2>&1 # clear any fence a previous step left
  note "staged: Ginger on '$DB', map 0, pad ($PAD_X $PAD_Y $PAD_Z)"

  # 2. restart WITH the injection armed.
  gw_start "$STEP" || { bad "injected gateway would not start"; MATRIX+=("FAIL $STEP"); continue; }

  # 3. drive the portal. A HELD session is required: the transfer runs inside the client's loading
  #    screen (the WORLDPORT_ACK handler), and `stay` drains with the decoding recv() that answers
  #    SMSG_NEW_WORLD. Deadline well past the whole window — `stay` exits rc 0 on timeout, silently.
  if ! stay_start TEST test123 Ginger 180; then
    bad "Ginger never went live before the portal — cannot attribute what follows to the injection"
    pkill -x wire-client 2>/dev/null; MATRIX+=("FAIL $STEP"); continue
  fi
  sleep 2
  scall debug_enter_areatrigger "$GINGER" $DM_TRIGGER

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

  # 5. THE CRASH-POINT INVARIANT, read straight off both databases.
  HW=$(has_char "$DB" "$GINGER");  HI=$(has_char "$IDB" "$GINGER")
  LW=$(live_on  "$DB" "$GINGER");  LI=$(live_on  "$IDB" "$GINGER")
  note "durable: $DB=$HW $IDB=$HI   live(unfenced): $DB=$LW $IDB=$LI"
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
  gw_start "" || { bad "clean gateway would not restart after the injected crash"; MATRIX+=("FAIL $STEP"); continue; }
  if timeout 90 "$WC" TEST test123 Ginger logout >/tmp/xcrash_reentry_$STEP.log 2>&1; then
    good "re-entry: a fresh wire session logged Ginger back into the world after the crash"
  else
    bad "re-entry: Ginger could NOT re-enter the world after a clean gateway restart (in-transit forever?) — see /tmp/xcrash_reentry_$STEP.log"
    tail -5 "/tmp/xcrash_reentry_$STEP.log" >&2
  fi
  HW=$(has_char "$DB" "$GINGER"); HI=$(has_char "$IDB" "$GINGER")
  LW=$(live_on "$DB" "$GINGER");  LI=$(live_on "$IDB" "$GINGER")
  if [ $(( LW + LI )) -eq 1 ]; then
    good "post-recovery: settled LIVE on exactly one database ($DB=$LW, $IDB=$LI)"
  else
    bad "post-recovery: expected exactly 1 LIVE copy after the restart+login, found $(( LW + LI )) (durable $DB=$HW/$IDB=$HI, live $DB=$LW/$IDB=$LI) — recovery did not settle"
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
echo "════════ transfer crash matrix ════════"
for r in "${MATRIX[@]}"; do printf "  %s\n" "$r"; done
echo "───────────────────────────────────────"
if [ "$FAILED" -eq 0 ]; then
  echo "[xcrash] PASS — all ${#STEPS[@]} crash points hold ZERO-LOSS + NO-DUPE and recover"
  exit 0
fi
echo "[xcrash] FAIL"
exit 1
