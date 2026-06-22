#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# protobuf_codegen.sh — proto codegen check (specs/01 §8.1).
# Decision (01 §8.1): proto bindings are generated via build.rs (tonic/prost) and
# are NOT committed, so this reduces to `buf lint` + `buf breaking` + "the build
# compiles". No proto/ tree exists until M2; this is a no-op until then.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

if [ ! -d proto ]; then
  echo "protobuf_codegen: no proto/ tree yet (lands in M2) — verifying the build only."
  cargo check --workspace --all-features
  exit 0
fi

# Lint/breaking run from the buf module dir (proto/buf.yaml) so buf scopes to the
# proto tree and applies the avalanche lint config — running from the repo root
# would make buf default-scan the whole tree (incl. the bazel symlink).
(
  cd proto
  buf lint
  # Guard wire compatibility against the base branch when available.
  if git rev-parse --verify --quiet master >/dev/null; then
    buf breaking --against '.git#branch=master,subdir=proto'
  fi
)
# Compile every crate that runs a proto build.rs (codegen happens during build).
cargo check --workspace --all-features
