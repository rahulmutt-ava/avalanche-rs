#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# check_coverage_floor.sh — per-crate coverage floor gate (specs/02 §12, X.8).
#
# Parses an lcov.info file (produced by the `test-coverage` / `coverage-floor`
# Taskfile task via `cargo llvm-cov nextest --lcov`) and enforces the committed
# per-crate line-coverage floors below ("a PR may not lower a crate below its
# floor", specs/02 §12: 90% protocol-critical / 80% VM / 70% glue).
#
# lcov format recap: per source file a `SF:<path>` record opens, `DA:<line>,<hits>`
# lines give per-line hit counts, and `end_of_record` closes it. We sum per-file
# covered/total DA lines, group files by crate (a path containing
# `crates/<crate-name>/` maps to crate `<crate-name>`), aggregate covered/total
# per crate, and compare 100*covered/total against the floor.
#
# A crate listed in FLOORS but with NO lines in this lcov (e.g. a scoped run that
# didn't include it) is SKIPPED with a warning, not failed — so partial coverage
# runs don't spuriously break the gate.
#
# Belongs in NIGHTLY (a full instrumented coverage build is too expensive for the
# per-PR `tests-required` aggregator). See `.github/workflows/nightly.yml`.
set -euo pipefail

LCOV="${1:-lcov.info}"
if [ ! -f "$LCOV" ]; then
  echo "check_coverage_floor: $LCOV not found — run './scripts/run_task.sh test-coverage' first." >&2
  exit 1
fi

# Per-crate floors (specs/02 §12: 90% protocol-critical / 80% VM / 70% glue).
#
# MEASURED-THEN-RATCHETED: each value is the measured line-% for that crate
# (scoped `cargo llvm-cov nextest -p <crate>`) rounded DOWN to the nearest 5 —
# never above measured, so floors pass today and are ratcheted UP as coverage
# improves. They are intentionally conservative; raise them as crates mature
# toward the §12 spec targets (90% protocol-critical / 80% VM / 70% glue).
# Re-measure with:
#   cargo llvm-cov nextest --lcov --output-path /tmp/cov.info -p <crate> ...
#   ./scripts/check_coverage_floor.sh /tmp/cov.info
#
# Measured 2026-06-17 (scoped run, this worktree):
#   ava-types   79% (281/354)  -> floor 75
#   ava-utils   66% (390/583)  -> floor 65
#   ava-version 82% (332/401)  -> floor 80
declare -A FLOORS=(
  [ava-types]=75
  [ava-utils]=65
  [ava-version]=80
)

if [ ${#FLOORS[@]} -eq 0 ]; then
  echo "check_coverage_floor: no floors configured yet (deepened in tier X / X.8) — skipping."
  exit 0
fi

# Parse lcov: emit "<crate> <covered> <total>" per crate that has DA lines.
# We accumulate per-crate sums in awk. Crate name = the path segment immediately
# after a `crates/` segment (robust to absolute paths and to the crate name
# appearing elsewhere in the path — we anchor on the literal `crates/` segment).
PARSED="$(
  awk '
    /^SF:/ {
      path = substr($0, 4)
      crate = ""
      n = split(path, parts, "/")
      for (i = 1; i < n; i++) {
        if (parts[i] == "crates") {
          crate = parts[i + 1]
          break
        }
      }
      next
    }
    /^DA:/ {
      if (crate == "") next
      # DA:<line>,<hits>
      rest = substr($0, 4)
      split(rest, da, ",")
      hits = da[2]
      total[crate]++
      if (hits + 0 > 0) covered[crate]++
      next
    }
    /^end_of_record/ { crate = ""; next }
    END {
      for (c in total) printf "%s %d %d\n", c, covered[c] + 0, total[c]
    }
  ' "$LCOV"
)"

# Build an associative lookup of measured crates -> "covered total".
declare -A MEASURED=()
while read -r crate cov tot; do
  [ -z "$crate" ] && continue
  MEASURED["$crate"]="$cov $tot"
done <<EOF
$PARSED
EOF

fail=0
declare -a SUMMARY=()

for crate in $(printf '%s\n' "${!FLOORS[@]}" | sort); do
  floor="${FLOORS[$crate]}"
  if [ -z "${MEASURED[$crate]:-}" ]; then
    echo "WARN: $crate not measured in $LCOV (no lines) — skipping floor check (floor ${floor}%)." >&2
    SUMMARY+=("$(printf '  %-20s   n/a   (floor %s%%, not measured)' "$crate" "$floor")")
    continue
  fi
  read -r cov tot <<<"${MEASURED[$crate]}"
  if [ "$tot" -eq 0 ]; then
    echo "WARN: $crate has 0 total lines in $LCOV — skipping floor check." >&2
    SUMMARY+=("$(printf '  %-20s   n/a   (floor %s%%, 0 lines)' "$crate" "$floor")")
    continue
  fi
  # Integer percentage, truncated (conservative — never rounds up over the floor).
  pct=$(( 100 * cov / tot ))
  if [ "$pct" -lt "$floor" ]; then
    echo "FAIL: $crate ${pct}% < floor ${floor}%  (${cov}/${tot} lines)"
    fail=1
    SUMMARY+=("$(printf '  %-20s  %3d%%  FAIL (floor %s%%, %s/%s)' "$crate" "$pct" "$floor" "$cov" "$tot")")
  else
    SUMMARY+=("$(printf '  %-20s  %3d%%  ok   (floor %s%%, %s/%s)' "$crate" "$pct" "$floor" "$cov" "$tot")")
  fi
done

echo
echo "check_coverage_floor: per-crate line coverage (source: $LCOV)"
for line in "${SUMMARY[@]}"; do
  echo "$line"
done

if [ "$fail" -ne 0 ]; then
  echo
  echo "check_coverage_floor: FAILED — one or more crates below floor (specs/02 §12)." >&2
  exit 1
fi

echo
echo "check_coverage_floor: all configured crates meet their floors."
exit 0
