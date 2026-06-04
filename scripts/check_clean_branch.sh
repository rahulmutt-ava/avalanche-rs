#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# check_clean_branch.sh — assert the git working tree is clean (specs/01 §5.2).
# Used by the dirty-tree gates (check-generate-*, bazel-check-metadata, deps-tidy)
# so CI fails any PR that regenerated an artifact without committing the result.
set -euo pipefail

if [ -n "$(git status --porcelain)" ]; then
  echo "ERROR: working tree is dirty after regenerating artifacts:" >&2
  git status --short >&2
  echo >&2
  echo "Commit the regenerated files (Cargo.lock / MODULE.bazel.lock / BUILD.bazel / ...)." >&2
  exit 1
fi
