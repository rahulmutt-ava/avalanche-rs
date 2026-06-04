#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# lint_saevm.sh — stricter clippy pass for SAE crates (specs/01 §7.4, 00 §7.7).
# Analogue of avalanchego's `gosec -include=G115` over vms/saevm/...: the cast_*
# lints are the direct integer-overflow-on-conversion analogue. No-op (success)
# until the first ava-saevm* crate lands (M7).
set -euo pipefail

SAE_CRATES=$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[].name | select(startswith("ava-saevm"))')

if [ -z "$SAE_CRATES" ]; then
  echo "lint-saevm: no ava-saevm* crates yet (lands in M7) — nothing to check."
  exit 0
fi

for crate in $SAE_CRATES; do
  cargo clippy -p "$crate" --all-targets --all-features -- \
    -D warnings \
    -W clippy::pedantic \
    -D clippy::arithmetic_side_effects \
    -D clippy::cast_possible_truncation \
    -D clippy::cast_sign_loss \
    -D clippy::cast_possible_wrap
done
