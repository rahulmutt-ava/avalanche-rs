#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# nix_run.sh — run a command inside the Nix dev shell when available (carried
# over from avalanchego).
#
# M0 STUB. X-cross-cutting (task X.1) adds flake.nix and the pinned dev shell.
# Until then, if `nix` and a flake are present we wrap the command in
# `nix develop --command`; otherwise we exec the command directly so the
# toolchain on PATH (e.g. an active dev shell or rustup) is used as-is.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ "$#" -eq 0 ]; then
  echo "nix_run.sh: no command given" >&2
  exit 2
fi

if command -v nix >/dev/null 2>&1 && [ -f "${REPO_ROOT}/flake.nix" ]; then
  # NIX_DEV_SHELL selects a non-default dev shell (e.g. `fuzz` for the nightly
  # toolchain used by cargo-fuzz). Empty/unset → the default stable shell.
  flake_ref="${REPO_ROOT}"
  if [ -n "${NIX_DEV_SHELL:-}" ]; then
    flake_ref="${REPO_ROOT}#${NIX_DEV_SHELL}"
  fi
  exec nix develop "${flake_ref}" --command "$@"
fi

exec "$@"
