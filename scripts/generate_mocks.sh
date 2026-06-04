#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# generate_mocks.sh — mock codegen check (specs/01 §8.2).
# mockall mocks are macro-generated at compile time via #[cfg_attr(test, automock)];
# nothing is committed, so this verifies the macro expansions compile. Essentially
# a no-op kept for task parity with avalanchego's generate-mocks.
set -euo pipefail

cargo check --workspace --all-features --tests
