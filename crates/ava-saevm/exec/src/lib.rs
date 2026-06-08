// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-exec` — the single-task streaming executor over the `ava-evm`
//! reuse APIs, plus the eventual receipt buffer (specs/11 §6).
//!
//! M7.1 scaffold: empty body behind the SAE stricter-lint bar; the
//! implementation lands in a later M7 task (see `plan/M7-saevm.md`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
