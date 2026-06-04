#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# install_nix.sh — install Nix with an OS-appropriate installer (specs/01 §3, §5.2).
# Carried over from avalanchego. Uses the Determinate Systems installer (enables
# flakes + the nix command by default). Idempotent: skips if nix is present.
set -euo pipefail

if command -v nix >/dev/null 2>&1; then
  echo "nix already installed: $(nix --version)"
  exit 0
fi

echo "Installing Nix (Determinate Systems installer, flakes enabled)..."
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix \
  | sh -s -- install --no-confirm

echo "Nix installed. Open a new shell, then run: nix develop  (or use .envrc / direnv)."
