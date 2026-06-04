#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# shellcheck.sh — run shellcheck over the repo's shell scripts (carried over from
# avalanchego). specs/01 §5.1 (lint-shell).
set -euo pipefail

mapfile -t files < <(git ls-files '*.sh')
if [ ${#files[@]} -eq 0 ]; then
  exit 0
fi
shellcheck -x "${files[@]}"
