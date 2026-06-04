#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# run_task.sh — the canonical task launcher (carried over from avalanchego).
#
# M0 STUB. X-cross-cutting (tasks X.1 / X.4) matures this to the verbatim Go
# behavior: it should prefer `task` (go-task) on PATH and wrap every task in the
# Nix dev shell. Until go-task + flake.nix land, this stub implements the core
# tasks (build / test-unit / test-unit-fast / lint) directly via cargo so the
# implementation loop has a working entrypoint. Any task not handled here is
# delegated to `task` if it is installed.
#
# Usage: ./scripts/run_task.sh <task> [-- extra args]
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NIX_RUN="${REPO_ROOT}/scripts/nix_run.sh"

run() { "${NIX_RUN}" "$@"; }

usage() {
  cat <<'EOF'
Tasks (M0 stub — see specs/01-development-environment.md §5 for the full set):
  build                Build the avalanchers binary (release)
  build-debug-checks   Build the workspace with overflow + debug assertions
  test-unit            Unit tests (all features, CI profile) + doctests
  test-unit-fast       Fast local unit tests
  lint                 clippy (deny warnings) + rustfmt --check
  lint-fix             clippy --fix + cargo fmt
  --list               Show this list

Tasks not listed here are delegated to `task` (go-task) if installed; the full
Taskfile.yml surface is owned by plan/X-cross-cutting.md.
EOF
}

task="${1:-default}"
shift || true

case "${task}" in
  default | --list | -l | help | --help | -h)
    usage
    ;;
  build)
    run cargo build -p avalanchers --release "$@"
    ;;
  build-debug-checks)
    run cargo build --workspace --profile ci "$@"
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
    if command -v task >/dev/null 2>&1; then
      exec task "${task}" "$@"
    fi
    echo "run_task.sh: unknown task '${task}' and go-task is not installed." >&2
    echo "Run './scripts/run_task.sh --list' for the M0 task set." >&2
    exit 2
    ;;
esac
