#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# seed_porting_matrix.sh — seed a crate's tests/PORTING.md from the Go package's
# test list (specs/02 §10.1, tier-X task X.20).
#
# SCAFFOLD: the full enumeration (`go test -list '.*' ./...` against the pinned
# avalanchego tree, mapping each Go test -> Rust counterpart) is owned by X.20.
# This documents the procedure and emits a starter table for a given Go package.
#
# Usage: seed_porting_matrix.sh <go-package-path> > crates/<crate>/tests/PORTING.md
set -euo pipefail

pkg="${1:-}"
if [ -z "$pkg" ]; then
  echo "usage: $0 <go-package-path>  (e.g. utils/hashing)" >&2
  echo "Owned by tier-X task X.20; requires the pinned avalanchego tree on disk." >&2
  exit 2
fi

cat <<EOF
# PORTING.md — $pkg

| Go test | Rust counterpart | Status |
|---|---|---|
EOF

# When the Go tree is available (AVALANCHEGO_SRC), enumerate real tests:
if [ -n "${AVALANCHEGO_SRC:-}" ] && command -v go >/dev/null 2>&1; then
  (cd "$AVALANCHEGO_SRC" && go test -list '.*' "./$pkg/..." 2>/dev/null) \
    | grep -E '^Test|^Fuzz' \
    | sort -u \
    | while IFS= read -r t; do echo "| $t | _todo_ | wip |"; done
else
  echo "| _seed from AVALANCHEGO_SRC=<go tree> (X.20)_ | _todo_ | wip |"
fi
