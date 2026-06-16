// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-upgrade` — the Go→Rust rolling-upgrade test harness (specs/02 §10.4;
//! specs/16 §5(8); specs/26 §7 moving min-compatible floor; specs/00 §4.4;
//! M9.17).
//!
//! Skeleton crate registered by the M9.17 prep commit. The previous-Go-binary
//! bring-up, per-node Go→Rust swap with Go-dir→RocksDB import (M9.16), the
//! activation-height barrier, the continuity/no-fork assertions reusing the
//! `ava-differential` `Observation`, and the offline / gated-live arm split are
//! filled in by task M9.17.
