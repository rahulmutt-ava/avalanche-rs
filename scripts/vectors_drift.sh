#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# vectors_drift.sh — re-extract golden vectors from the pinned Go commit and diff
# against the committed corpus (specs/22 §7, tier-X task X.12).
#
# SCAFFOLD: the extraction harness (tools/extract-vectors) and `cargo xtask
# vectors diff` are owned by tier-X tasks X.10–X.12. This documents the flow:
#
#   1. read manifest.avalanchego_revision from tests/vectors/manifest.json
#   2. check out that avalanchego commit
#   3. go run ./tools/extract-vectors --out $TMP
#   4. cargo xtask vectors diff --against $TMP   (fails on any byte drift)
#
# A deliberate protocol change is: bump the revision -> `xtask vectors regen` ->
# review the vector diff in the PR.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

manifest="tests/vectors/manifest.json"
if [ ! -f "$manifest" ]; then
  echo "vectors_drift: $manifest not found." >&2
  exit 1
fi

echo "vectors_drift: re-extraction + diff is owned by tier-X tasks X.10–X.12."
echo "vectors_drift: pinned avalanchego revision:"
if command -v jq >/dev/null 2>&1; then
  jq -r '.avalanchego_revision // "<unset>"' "$manifest"
else
  grep -o '"avalanchego_revision"[^,]*' "$manifest" || echo "<unset>"
fi
