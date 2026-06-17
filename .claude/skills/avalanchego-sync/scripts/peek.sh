#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# avalanchego-sync :: peek
#
# Read-only diagnosis of upstream drift between the spec pin recorded in
# specs/README.md and the HEAD of the local ~/avalanchego checkout. Performs
# NO writes to either repo — it only reports what would need folding.
#
# Usage:
#   scripts/peek.sh [path-to-avalanchego-checkout]   # defaults to ~/avalanchego
#
# NOTE: this wants the avalanchego *source checkout* (a git dir), NOT the
# AVALANCHEGO_PATH env var — that conventionally points at a built *binary*
# (often the Rust avalanchers, for live tests), so it is deliberately ignored.
#
# Output sections:
#   1. The pin recorded in specs/README.md (generated-from + reviewed-through).
#   2. The checkout HEAD (commit, date, branch).
#   3. The drift range reviewed-through..HEAD as a oneline log (the review queue).
#   4. The subset of that range that touches SAE / C-Chain / EVM paths (the
#      active stricter area), with per-commit file lists to ease spec mapping.
set -euo pipefail

REPO_ROOT="$(git rev-parse --show-toplevel)"
README="$REPO_ROOT/specs/README.md"
SRC="${1:-$HOME/avalanchego}"

if [ ! -d "$SRC/.git" ]; then
  echo "ERROR: avalanchego checkout not found at '$SRC'" >&2
  echo "       pass the path as arg 1 or set AVALANCHEGO_PATH" >&2
  exit 1
fi
if [ ! -f "$README" ]; then
  echo "ERROR: $README not found (run from inside the avalanche-rs repo)" >&2
  exit 1
fi

# --- 1. the recorded pin ----------------------------------------------------
# The provenance block wraps the sha onto the next blockquote line, so flatten
# newlines and the leading "> " markers into spaces before matching.
FLAT="$(tr '\n>' '  ' < "$README")"
# "generated from avalanchego commit `<sha>`"
GENERATED="$(grep -oE 'avalanchego commit[[:space:]]+`?[0-9a-f]{7,40}' <<<"$FLAT" | head -1 | grep -oE '[0-9a-f]{7,40}' || true)"
# "Upstream commits through `<sha>`"
PIN="$(grep -oE 'commits through[[:space:]]+`?[0-9a-f]{7,40}' <<<"$FLAT" | head -1 | grep -oE '[0-9a-f]{7,40}' || true)"

echo "=== spec pin (specs/README.md) ==="
echo "  generated-from : ${GENERATED:-<not found>}"
echo "  reviewed-through: ${PIN:-<not found>}"
echo

# --- 2. checkout HEAD -------------------------------------------------------
HEAD_FULL="$(git -C "$SRC" rev-parse HEAD)"
echo "=== ~/avalanchego HEAD ($SRC) ==="
echo "  branch: $(git -C "$SRC" rev-parse --abbrev-ref HEAD)"
echo "  commit: $HEAD_FULL"
git -C "$SRC" log -1 --format='  date  : %ci%n  title : %s'
echo

if [ -z "$PIN" ]; then
  echo "WARN: could not parse reviewed-through pin; cannot compute drift range." >&2
  exit 0
fi

if ! git -C "$SRC" cat-file -e "${PIN}^{commit}" 2>/dev/null; then
  echo "WARN: pin '$PIN' is not a commit in $SRC — the checkout may be too shallow" >&2
  echo "      or on a divergent branch. Try 'git -C $SRC fetch --unshallow'." >&2
  exit 0
fi

RANGE="${PIN}..${HEAD_FULL}"
COUNT="$(git -C "$SRC" rev-list --count "$RANGE")"

# --- 3. full drift (the review queue) --------------------------------------
echo "=== drift to review: ${PIN:0:10}..${HEAD_FULL:0:10}  ($COUNT commits) ==="
if [ "$COUNT" -eq 0 ]; then
  echo "  (none — specs are in sync with the checkout HEAD)"
  exit 0
fi
git -C "$SRC" log --oneline --no-decorate "$RANGE"
echo

# --- 4. SAE / C-Chain / EVM subset (the active stricter area) --------------
SAE_PATHS=(vms/saevm vms/evm coreth params/network_upgrades.go)
echo "=== SAE / C-Chain / EVM-touching commits in range (active area) ==="
SAE_SHAS="$(git -C "$SRC" log --format='%H' "$RANGE" -- "${SAE_PATHS[@]}")"
if [ -z "$SAE_SHAS" ]; then
  echo "  (none touch the SAE/C-Chain/EVM paths — drift is elsewhere; still triage section 3)"
else
  while IFS= read -r sha; do
    [ -z "$sha" ] && continue
    git -C "$SRC" log -1 --format='--- %h  (%cs)  %s' "$sha"
    git -C "$SRC" show --stat --format='' "$sha" -- "${SAE_PATHS[@]}" | sed 's/^/      /'
  done <<< "$SAE_SHAS"
fi
