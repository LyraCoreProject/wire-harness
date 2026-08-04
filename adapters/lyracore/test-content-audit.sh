#!/usr/bin/env bash
# test-content-audit.sh — the 1-20 CONTENT regression gate (2026-07-17). Runs the two audit reducers
# (debug_audit_class_kits + debug_audit_quest_chains) and asserts the leveling path stayed healthy:
#   1. every class's CORE rotation is live at L1-9 (zero hollow in the first band — the primaries),
#   2. total completable quests through L20 stays at/above a floor,
#   3. the hollow-active count stays at/below the known utility tail.
# This is the net that makes a bigger content import SAFE: a future import that breaks a quest chain
# (missing giver / bad objective) or a class ability (unmapped effect) fails HERE, headlessly.
set -uo pipefail
source "$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/adapter-env.sh" # two roots; cds to $LYRACORE_DIR
DB=lyracore
FAILED=0
ok()   { echo "[audit] OK: $*"; }
fail() { echo "[audit] FAIL: $*" >&2; FAILED=1; }

# Thresholds — the floor the 2026-07-17 baseline established (156 completable; L1-9 zero-hollow;
# 6 hollow-active utility spells). A regression drops below these.
MIN_COMPLETABLE=150
MAX_HOLLOW=8   # 6 known utility hollows + slack; a broken class ability pushes past this

spacetime call "$DB" debug_audit_class_kits >/dev/null 2>&1
spacetime call "$DB" debug_audit_quest_chains >/dev/null 2>&1
sleep 2
LOG=$(spacetime logs "$DB" -n 200 2>/dev/null)

# --- 1. L1-9 core rotations all live (zero hollow in the first band, every class) ---
# The class lines read "Class   L1-9: NL/MH/PN  ...". Extract the L1-9 hollow (the M in NL/MH).
L1_HOLLOW=$(echo "$LOG" | grep -oE "L1-9: [0-9]+L/[0-9]+H" | grep -oE "/[0-9]+H" | grep -oE "[0-9]+" | awk '{s+=$1} END{print s+0}')
if [ "${L1_HOLLOW:-1}" -eq 0 ]; then ok "L1-9 core rotations: every class 0 hollow"; else fail "L1-9 hollow total = $L1_HOLLOW (a core low-level ability regressed)"; fi

# --- 2. completable quests floor (dash-agnostic: sum the three band counts on the OK-quests line) ---
OKLINE=$(echo "$LOG" | grep "OK quests" | tail -1)
TOTAL=$(echo "$OKLINE" | grep -oE "L1-9: [0-9]+|L10-19: [0-9]+|L20: [0-9]+" | grep -oE "[0-9]+" | awk '{s+=$1} END{print s+0}')
if [ "${TOTAL:-0}" -ge "$MIN_COMPLETABLE" ]; then ok "completable quests = $TOTAL (>= $MIN_COMPLETABLE)"; else fail "completable quests = $TOTAL (< $MIN_COMPLETABLE — a quest chain broke)"; fi

# --- 3. hollow-active count ceiling ---
HOLLOW=$(echo "$LOG" | grep -cE "HOLLOW (Warrior|Paladin|Rogue|Priest|Mage|Warlock) ")
if [ "${HOLLOW:-99}" -le "$MAX_HOLLOW" ]; then ok "hollow-active spells = $HOLLOW (<= $MAX_HOLLOW)"; else fail "hollow-active spells = $HOLLOW (> $MAX_HOLLOW — a class ability went hollow)"; fi

if [ "$FAILED" = "0" ]; then echo "[content-audit] PASS — 1-20 leveling health intact ($TOTAL quests, $HOLLOW hollow)"; else echo "[content-audit] FAIL"; exit 1; fi
