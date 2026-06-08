// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-saevm-intmath` — SAE checked integer math (`mul_div`, `mul_div_ceil`,
//! `ceil_div`, bounded add/sub/mul via a `U256` intermediate; specs/11 §6,
//! specs/21 §6/§8).
//!
//! M7.1 scaffold: empty body behind the SAE stricter-lint bar; the
//! implementation lands in a later M7 task (see `plan/M7-saevm.md`).

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]
#![deny(clippy::arithmetic_side_effects)]
