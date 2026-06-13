#!/usr/bin/env bash
# Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
# See the file LICENSE for licensing terms.
#
# single_runtime_lint.sh — enforce the single-runtime rule (specs/17 §1.1,
# 00 §7.2): there is exactly one tokio runtime in the process and the
# `avalanchers` binary owns it. No library crate may construct a runtime or
# call `block_on` (nested runtimes panic and break cancellation).
#
# Forbids `Runtime::new`, `new_multi_thread`, `new_current_thread`, and
# `block_on` everywhere EXCEPT the `avalanchers` binary entrypoint
# (crates/avalanchers/src/main.rs) and test code (`tests/`, `#[cfg(test)]`
# files, benches). This is the analogue of avalanchego's grep CI gates
# (cf. scripts/tau_lint.sh / tausecondslint).
set -euo pipefail

# Patterns that build or block on a runtime. `block_on` is the load-bearing
# one (a sub-runtime is the only way to call it from library code).
pattern='Runtime::new|new_multi_thread|new_current_thread|\.block_on\(|block_on!'

# Search Rust sources under crates/, excluding:
#   - the binary entrypoint (it legitimately owns the one runtime),
#   - any tests/ directory (integration tests may drive a runtime),
#   - files whose path marks them as test/bench helpers,
#   - the rpcchainvm-plugin *client* bridges (allowlist below).
#
# Allowlist — the synchronous `Database`/`SharedMemory` client bridges (specs/17
# §1.2): these implement a *blocking* trait over a tonic gRPC channel for a VM
# plugin running in its **own subprocess**. The blocking trait cannot accept a
# `Handle`, so each bridge owns a tiny current-thread runtime to drive its RPCs.
# This is per-plugin-process, NOT a second runtime in the node process, so it
# does not violate the single-runtime rule. New entries need a spec rationale.
allowlist='crates/avalanchers/src/main\.rs|crates/ava-database/src/rpcdb/client\.rs|crates/ava-vm-rpc/src/proxy/rpcdb\.rs|crates/ava-vm-rpc/src/proxy/sharedmemory\.rs'

violations=$(
  grep -rnE "$pattern" --include='*.rs' crates/ \
    | grep -vE "$allowlist" \
    | grep -vE '/tests/' \
    | grep -vE '/benches/' \
    || true
)

# Drop lines inside test modules / test helpers (best-effort: a line whose file
# is only reachable under cfg(test) is allowed). We keep this simple and only
# whitelist obvious test files by name; anything else is a real violation.
violations=$(printf '%s\n' "$violations" | grep -vE '(testutil|test_util|_test\.rs|/tests?\.rs)' || true)

if [ -n "$violations" ]; then
  echo "ERROR: single-runtime rule violated (specs/17 §1.1)." >&2
  echo "Only crates/avalanchers/src/main.rs may build/block-on a tokio runtime;" >&2
  echo "library crates must spawn onto the ambient runtime via the passed Handle." >&2
  echo >&2
  printf '%s\n' "$violations" >&2
  exit 1
fi

echo "single_runtime_lint: OK — only the avalanchers binary owns a runtime."
