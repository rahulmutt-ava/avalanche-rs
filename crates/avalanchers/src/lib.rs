// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `avalanchers` node library — node-assembly wiring shared by the binary
//! and exercised by the integration tests.
//!
//! The binary (`src/main.rs`) is a thin entrypoint over this crate; the wiring
//! modules under [`wiring`] assemble already-built crates into a running node as
//! they land in successive milestones. [`app`] is the Rust port of Go
//! `app/app.go` (banner, chmod, fd-limit, signals, version helper).

// `app::set_fd_limit` carries one isolated `libc::setrlimit` FFI call on unix; it
// re-enables `unsafe` only on that single block. The crate forbids it everywhere
// else (`deny` on unix so the scoped `#[allow]` is honored; `forbid` elsewhere).
#![cfg_attr(unix, deny(unsafe_code))]
#![cfg_attr(not(unix), forbid(unsafe_code))]

pub mod app;
pub mod wiring;
