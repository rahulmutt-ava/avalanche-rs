// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Library surface for `cargo xtask` automation modules that are exercised by
//! integration tests (`xtask/tests/`).
//!
//! Only the determinism-audit pass is published here today (it has a hermetic
//! fixture test, X.19); the remaining subcommands stay private to the binary.

#![forbid(unsafe_code)]

// `clap` is a workspace dependency used by the binary target, not the library.
use clap as _;

pub mod lint_determinism;
