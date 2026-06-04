#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# check_coverage_floor.sh — per-crate coverage floor gate (specs/02 §12, X.8).
# SCAFFOLD: parses lcov.info (produced by `test-coverage`) against the committed
# per-crate floor table below ("a PR may not lower a crate below its floor").
# The floor table is filled in per milestone as crates land; M0 crates are added
# here as they reach the buildable-&-green gate.
set -euo pipefail

LCOV="${1:-lcov.info}"
if [ ! -f "$LCOV" ]; then
  echo "check_coverage_floor: $LCOV not found — run './scripts/run_task.sh test-coverage' first." >&2
  exit 1
fi

# Per-crate floors (02 §12: 90% protocol-critical / 80% VM / 70% glue).
# TODO(X.8): populate as crates land; deepened each milestone.
declare -A FLOORS=(
  # [ava-codec]=90
  # [ava-crypto]=90
  # [ava-types]=90
  # [ava-utils]=90
  # [ava-version]=80
)

if [ ${#FLOORS[@]} -eq 0 ]; then
  echo "check_coverage_floor: no floors configured yet (deepened in tier X / X.8) — skipping."
  exit 0
fi

echo "check_coverage_floor: floor enforcement not yet implemented (X.8 deepens this)." >&2
exit 0
