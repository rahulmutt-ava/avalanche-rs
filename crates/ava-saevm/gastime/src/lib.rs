// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-gastime` — the SAE gas clock: Tau-pinned rate, excess scaling,
//! and the ACP-176 price (specs/11 §2.2, specs/21 §6).
//!
//! M7.1 scaffold: empty body behind the SAE stricter-lint bar; the
//! implementation lands in a later M7 task (see `plan/M7-saevm.md`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]
