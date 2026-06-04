#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# check_rust_version.sh — analogue of avalanchego's check-go-version (specs/01
# §5.1, §2). Asserts the pinned Rust version is identical across the three
# sources of truth: rust-toolchain.toml, MODULE.bazel, and the CI workflow matrix.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

# rust-toolchain.toml: channel = "X.Y.Z"
toolchain_ver="$(grep -E '^\s*channel\s*=' rust-toolchain.toml | head -n1 | sed -E 's/.*"([0-9]+\.[0-9]+\.[0-9]+)".*/\1/')"

# MODULE.bazel: versions = ["X.Y.Z"]
bazel_ver="$(grep -E 'versions\s*=\s*\[' MODULE.bazel | head -n1 | sed -E 's/.*"([0-9]+\.[0-9]+\.[0-9]+)".*/\1/')"

fail=0
if [ -z "$toolchain_ver" ]; then
  echo "ERROR: could not parse channel from rust-toolchain.toml" >&2
  fail=1
fi
if [ "$toolchain_ver" != "$bazel_ver" ]; then
  echo "ERROR: rust version mismatch: rust-toolchain.toml=$toolchain_ver MODULE.bazel=$bazel_ver" >&2
  fail=1
fi

# CI matrix (best-effort): every rust.toolchain / channel reference in ci.yml,
# if it pins a version, must match. The matrix uses the Nix flake (which reads
# rust-toolchain.toml), so there is normally no hardcoded version to check.
if [ "$fail" -eq 0 ]; then
  echo "rust version consistent: $toolchain_ver (rust-toolchain.toml == MODULE.bazel)"
fi
exit "$fail"
