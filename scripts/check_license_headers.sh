#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# check_license_headers.sh — analogue of `go-license --verify` (specs/01 §7.6).
# Asserts every tracked .rs file begins with the Ava Labs license header.
# Generated files (*.pb.rs, *mock_*.rs) and anything under target/ are exempt,
# mirroring the Go exemptions for *.pb.go / mock_*.go.
set -euo pipefail

HEADER='// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.'
missing=0
while IFS= read -r f; do
  case "$f" in
    *.pb.rs | *mock_*.rs | */target/*) continue ;;
  esac
  if ! head -n1 "$f" | grep -qF "$HEADER"; then
    echo "missing license header: $f"
    missing=1
  fi
done < <(git ls-files '*.rs')

exit "$missing"
