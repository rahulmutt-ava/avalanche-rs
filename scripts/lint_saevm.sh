#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# lint_saevm.sh — stricter clippy pass for SAE crates (specs/01 §7.4, 00 §7.7).
# Analogue of avalanchego's `gosec -include=G115` over vms/saevm/...: the cast_*
# lints are the direct integer-overflow-on-conversion analogue. No-op (success)
# until the first ava-saevm* crate lands (M7).
#
# NOTE: `--no-deps` is REQUIRED. The `-- -D <lint>` flags after `--` are appended
# to every clippy-driver invocation in the build graph, so without `--no-deps`
# they leak onto transitively-compiled NON-SAE workspace members (e.g. ava-utils,
# pulled in via ava-vm/ava-snow once an SAE crate like the adaptor/gastime/types
# depends on the rest of the workspace), failing on arithmetic the stricter SAE
# bar was never meant to police outside vms/saevm. `--no-deps` confines clippy
# analysis to the SAE crate itself; its deps are still compiled, just not linted
# here (they answer to the normal repo lint pass). Discovered in M7.10.
set -euo pipefail

SAE_CRATES=$(cargo metadata --no-deps --format-version 1 \
  | jq -r '.packages[].name | select(startswith("ava-saevm"))')

if [ -z "$SAE_CRATES" ]; then
  echo "lint-saevm: no ava-saevm* crates yet (lands in M7) — nothing to check."
  exit 0
fi

for crate in $SAE_CRATES; do
  cargo clippy -p "$crate" --all-targets --all-features --no-deps -- \
    -D warnings \
    -W clippy::pedantic \
    -D clippy::arithmetic_side_effects \
    -D clippy::cast_possible_truncation \
    -D clippy::cast_sign_loss \
    -D clippy::cast_possible_wrap
done
