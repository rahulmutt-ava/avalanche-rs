// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-db` — the SAE storage tracker: consensus-versus-execution state
//! over Firewood revisions (CC-ORDER, no reorgs; specs/11 §7, specs/27 §2.4).
//!
//! M7.1 scaffold: empty body behind the SAE stricter-lint bar; the
//! implementation lands in a later M7 task (see `plan/M7-saevm.md`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
