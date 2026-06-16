// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Crash-injection hardening harness (specs/27 §9, §2 CC-ATOMIC, §3.1 two-sided
//! shared-memory consistency; plan/M9 §M9.20).
//!
//! SCAFFOLD: this module hosts the `FailpointDb` wrapper (errors/aborts on the
//! N-th `write`) and the out-of-process crash/restart harness used to prove that
//! block acceptance is atomic (every accepted block is fully present or fully
//! absent on restart) and that an X→P / X→C export crashed inside the
//! `[SM-replay, write)` window observes all-or-nothing on the peer chain. The
//! offline arm asserts the Rust node's recovery is itself all-or-nothing and
//! idempotent; the live Go-oracle-equivalence arm is gated behind the `live`
//! Cargo feature + `#[ignore]` (filled in by M9.20).
