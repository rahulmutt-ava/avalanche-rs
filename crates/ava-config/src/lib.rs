// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-config` — node configuration (specs 12 §1, 13).
//!
//! Mirrors the Go `config/` package: the verbatim 206-flag catalog
//! ([`keys`], [`flags::FLAG_SPECS`]), the programmatic clap [`flags::build_command`]
//! builder (flag names are data so the `golden::flag_parity` test can enumerate
//! them), and the viper-parity layered precedence resolver
//! (`CLI flag > env (AVAGO_*) > config file > built-in default`).

#![forbid(unsafe_code)]

pub mod defaults;
pub mod duration;
pub mod error;
pub mod flags;
pub mod keys;

pub use error::{ConfigError, Result};
