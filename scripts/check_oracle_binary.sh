#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# check_oracle_binary.sh — guard against running live/recorded-oracle gates
# against a STALE Go `avalanchego` binary.
#
# A live differential/interop gate (and the recorded-oracle emitters) is only
# meaningful if the built binary reflects the source tree it claims to. The
# `~/avalanchego` checkout HEAD is the upstream oracle pin; this script asserts
# the BINARY's embedded commit equals that HEAD, so you never compare Rust
# against a Go binary built from a different (e.g. pre-`git pull`) commit.
#
# It also WARNs (non-fatal) when the checkout HEAD differs from the golden-vector
# corpus pin (`tests/vectors/manifest.json:avalanchego_revision`) — relevant to
# `vectors-drift` re-extraction, not to live consensus roots.
#
# Usage:  ./scripts/check_oracle_binary.sh
# Env:    AVALANCHEGO_SRC  (default ~/avalanchego)
#         AVALANCHEGO_BIN  (default $AVALANCHEGO_SRC/build/avalanchego)
# Exit:   0 = binary matches checkout HEAD; 1 = mismatch / missing; rebuild needed.

set -euo pipefail

SRC="${AVALANCHEGO_SRC:-$HOME/avalanchego}"
BIN="${AVALANCHEGO_BIN:-$SRC/build/avalanchego}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

fail() { echo "FAIL: $*" >&2; exit 1; }

[ -d "$SRC/.git" ] || fail "no avalanchego checkout at $SRC (set AVALANCHEGO_SRC)"
[ -x "$BIN" ] || fail "no avalanchego binary at $BIN — build it: (cd $SRC && ./scripts/build.sh)"

HEAD_COMMIT="$(git -C "$SRC" rev-parse HEAD)"
BIN_COMMIT="$("$BIN" --version 2>/dev/null | grep -oE 'commit=[0-9a-f]+' | head -1 | cut -d= -f2)"

[ -n "$BIN_COMMIT" ] || fail "could not read commit= from '$BIN --version'"

# The binary embeds the short or full commit; compare on the binary's length.
HEAD_TRUNC="${HEAD_COMMIT:0:${#BIN_COMMIT}}"

if [ "$BIN_COMMIT" != "$HEAD_TRUNC" ]; then
  echo "FAIL: oracle binary is STALE." >&2
  echo "  binary commit : $BIN_COMMIT" >&2
  echo "  checkout HEAD : $HEAD_COMMIT" >&2
  echo "  Rebuild before trusting live/oracle roots: (cd $SRC && ./scripts/build.sh)" >&2
  exit 1
fi

# Non-fatal: surface drift vs the golden-vector corpus pin.
MANIFEST="$REPO_ROOT/tests/vectors/manifest.json"
if [ -f "$MANIFEST" ] && command -v jq >/dev/null 2>&1; then
  VEC_PIN="$(jq -r '.avalanchego_revision // empty' "$MANIFEST")"
  if [ -n "$VEC_PIN" ] && [ "${VEC_PIN:0:${#BIN_COMMIT}}" != "$BIN_COMMIT" ]; then
    echo "WARN: checkout HEAD ($HEAD_TRUNC) != vectors corpus pin (${VEC_PIN:0:${#BIN_COMMIT}})." >&2
    echo "      OK for live consensus roots; relevant only to vectors-drift re-extraction." >&2
  fi
fi

# Sanity: the rpcchainvm protocol version must stay at the supported pin.
if ! "$BIN" --version 2>/dev/null | grep -q 'rpcchainvm=45'; then
  fail "binary rpcchainvm protocol != 45 (interop pin); check upstream-delta"
fi

echo "OK: oracle binary matches checkout HEAD ($BIN_COMMIT), rpcchainvm=45."
