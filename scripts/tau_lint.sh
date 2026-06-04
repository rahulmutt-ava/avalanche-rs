#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# tau_lint.sh — analogue of avalanchego's tausecondslint (specs/01 §10, CLAUDE.md).
# Forbids raw `as` casts and unchecked Duration arithmetic on `Tau` quantities in
# SAE code. Use params::TAU + checked_* instead. No-op until ava-saevm* lands (M7).
set -euo pipefail

shopt -s nullglob
sae_dirs=(crates/ava-saevm*)
if [ ${#sae_dirs[@]} -eq 0 ]; then
  echo "tau_lint: no ava-saevm* crates yet (lands in M7) — nothing to check."
  exit 0
fi

if grep -rnE '\bTauSeconds\b.*\bas\b|Instant::now\(\)[[:space:]]*\+[[:space:]]*[^;]*TauSeconds' \
     --include='*.rs' "${sae_dirs[@]}"; then
  echo "ERROR: use a typed Duration (params::TAU), never raw casts on TauSeconds"
  exit 1
fi
