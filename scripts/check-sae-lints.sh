#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# check-sae-lints.sh — structural guard for the SAE stricter-lint bar (M7.1,
# specs/11 §3, 00 §7.7). Greps each crates/ava-saevm/*/src/lib.rs for the
# required inner attributes + license header so the bar cannot silently rot when
# a new SAE crate is added. The behavioural gate is scripts/lint_saevm.sh (clippy
# pedantic + arithmetic_side_effects + cast_* deny); this is the cheap pre-check.
set -euo pipefail

# The gas-time crates additionally deny arithmetic_side_effects at the crate
# level (specs/11 §2; the gas clock advances only by checked gas math).
ARITH_CRATES=" intmath proxytime gastime gasprice "

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
sae_dir="${repo_root}/crates/ava-saevm"

shopt -s nullglob
libs=("${sae_dir}"/*/src/lib.rs)
if [ ${#libs[@]} -eq 0 ]; then
  echo "check-sae-lints: no crates/ava-saevm/*/src/lib.rs found — nothing to check." >&2
  exit 1
fi

fail=0
for lib in "${libs[@]}"; do
  crate_dir="$(dirname "$(dirname "$lib")")"
  crate="$(basename "$crate_dir")"

  if ! grep -qF '// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.' "$lib"; then
    echo "MISSING license header: $lib" >&2
    fail=1
  fi
  if ! grep -qF '#![forbid(unsafe_code)]' "$lib"; then
    echo "MISSING #![forbid(unsafe_code)]: $lib" >&2
    fail=1
  fi
  if ! grep -qF '#![warn(clippy::pedantic)]' "$lib"; then
    echo "MISSING #![warn(clippy::pedantic)]: $lib" >&2
    fail=1
  fi
  if [[ "$ARITH_CRATES" == *" $crate "* ]]; then
    if ! grep -qF '#![deny(clippy::arithmetic_side_effects)]' "$lib"; then
      echo "MISSING #![deny(clippy::arithmetic_side_effects)] (gas-time crate): $lib" >&2
      fail=1
    fi
  fi
done

if [ "$fail" -ne 0 ]; then
  echo "check-sae-lints: FAILED — fix the SAE stricter-lint bar above." >&2
  exit 1
fi

echo "check-sae-lints: OK (${#libs[@]} ava-saevm crates pass the structural lint bar)."
