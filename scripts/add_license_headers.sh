#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# add_license_headers.sh — the non-`--verify` counterpart of
# check_license_headers.sh (specs/01 §7.6). Prepends the two-line Ava Labs
# license header to any tracked .rs file missing it. Idempotent.
set -euo pipefail

read -r -d '' HEADER <<'EOF' || true
// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.
EOF

FIRST='// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.'
while IFS= read -r f; do
  case "$f" in
    *.pb.rs | *mock_*.rs | */target/*) continue ;;
  esac
  if ! head -n1 "$f" | grep -qF "$FIRST"; then
    printf '%s\n\n' "$HEADER" | cat - "$f" >"$f.tmp" && mv "$f.tmp" "$f"
    echo "added license header: $f"
  fi
done < <(git ls-files '*.rs')
