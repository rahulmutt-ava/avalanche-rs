#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# bazel_workspace_status.sh — release stamping (specs/01 §4.4).
# Emits STABLE_* keys consumed by `bazel build --config=release --stamp`.
set -euo pipefail

echo "STABLE_GIT_COMMIT $(git rev-parse HEAD 2>/dev/null || echo unknown)"
echo "STABLE_GIT_DIRTY $([ -n "$(git status --porcelain 2>/dev/null)" ] && echo 1 || echo 0)"
echo "STABLE_BUILD_TIME $(date -u +%Y-%m-%dT%H:%M:%SZ)"
