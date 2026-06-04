#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# run_task.sh — the canonical task launcher (carried over from avalanchego;
# specs/01-development-environment.md §5).
#
# Behavior (X.1/X.4): prefer `task` (go-task) and run it inside the Nix dev shell
# so every task gets the pinned toolchain. The full task surface lives in
# Taskfile.yml. If go-task is unavailable (no Nix, no `task` on PATH), fall back
# to a small set of cargo stubs so the core inner loop still works.
#
# Usage: ./scripts/run_task.sh <task> [-- extra args]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NIX_RUN="${REPO_ROOT}/scripts/nix_run.sh"

run() { "${NIX_RUN}" "$@"; }

# Is go-task reachable, either directly or inside the Nix dev shell?
have_task() {
  command -v task >/dev/null 2>&1 && return 0
  run task --version >/dev/null 2>&1 && return 0
  return 1
}

usage() {
  if have_task; then
    run task --list-all
    return
  fi
  cat <<'EOF'
Tasks (fallback stub — install go-task / nix for the full Taskfile surface):
  build                Build the avalanchers binary (release)
  build-debug-checks   Build the workspace with overflow + debug assertions
  test-unit            Unit tests (all features, CI profile) + doctests
  test-unit-fast       Fast local unit tests
  lint                 clippy (deny warnings) + rustfmt --check
  lint-fix             clippy --fix + cargo fmt
  --list               Show this list

The full task surface (bazel-*, deps-*, generate-*, lint-all*, vectors-*, …)
is defined in Taskfile.yml and runs once go-task is available.
EOF
}

task="${1:-default}"
shift || true

# Preferred path: delegate to go-task (wrapped in the Nix dev shell).
if have_task; then
  case "${task}" in
    default | --list | -l)
      run task --list-all
      ;;
    help | --help | -h)
      run task --list-all
      ;;
    *)
      run task "${task}" "$@"
      ;;
  esac
  exit 0
fi

# Fallback: go-task is not installed and Nix is unavailable. Implement the core
# inner-loop tasks directly so contributors are never fully blocked.
case "${task}" in
  default | --list | -l | help | --help | -h)
    usage
    ;;
  build)
    run cargo build -p avalanchers --release "$@"
    ;;
  build-debug-checks)
    run cargo build --workspace --profile dev-checks "$@"
    ;;
  test-unit)
    if run cargo nextest --version >/dev/null 2>&1; then
      run cargo nextest run --workspace --all-features --profile ci "$@"
    else
      echo "cargo-nextest not found; falling back to cargo test" >&2
      run cargo test --workspace --all-features "$@"
    fi
    run cargo test --doc --workspace --all-features
    ;;
  test-unit-fast)
    if run cargo nextest --version >/dev/null 2>&1; then
      run cargo nextest run --workspace "$@"
    else
      run cargo test --workspace "$@"
    fi
    ;;
  lint)
    run cargo clippy --workspace --all-targets --all-features -- -D warnings
    run cargo fmt --all -- --check
    ;;
  lint-fix)
    run cargo clippy --workspace --all-targets --all-features --fix --allow-dirty --allow-staged
    run cargo fmt --all
    ;;
  *)
    echo "run_task.sh: unknown task '${task}' and go-task is not installed." >&2
    echo "Install go-task (or run inside 'nix develop') for the full Taskfile surface." >&2
    exit 2
    ;;
esac
