// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-hook` — SAE lifecycle hooks: the `Points`/`BlockBuilder` trait
//! seam, the mint/burn `Op`, and the `Settled` gas clock (specs/11 §9.1).
//!
//! M7.1 scaffold: empty body behind the SAE stricter-lint bar; the
//! implementation lands in a later M7 task (see `plan/M7-saevm.md`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
