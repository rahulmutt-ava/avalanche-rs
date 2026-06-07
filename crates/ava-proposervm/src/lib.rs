// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-proposervm` — the ProposerVM wrapper layer.
//!
//! Tier T2b (consensus). Owning spec: `specs/06-consensus.md` §7,
//! `specs/07-vm-framework.md` §7.1-§7.3. This crate implements:
//!
//! - [`block`] — byte-exact ProposerVM block formats (statelessBlock /
//!   option / statelessGraniteBlock), the linear codec registration order
//!   (`statelessBlock(0)` / `option(1)` / `statelessGraniteBlock(2)`), the
//!   block-ID rule (`sha256` of the unsigned bytes, stripping the
//!   length-prefixed signature suffix), and `Header` signing (M3.21).
//! - [`proposer`] — the windower (gonum MT19937/-64 seeding, pre/post-Durango
//!   proposer scheduling) reusing the vendored `ava_utils::rng` MT and the
//!   `ava_utils::sampler` weighted-without-replacement sampler (M3.22).
//!
//! Byte-for-byte parity with Go `vms/proposervm/block` and
//! `vms/proposervm/proposer` is the contract (R1 confirmation on the windower).

#![forbid(unsafe_code)]

// These crates are consumed by the windower (`proposer`, M3.22); keep the
// dependency edges without an unused-crate warning until that lands.
use async_trait as _;
use ava_utils as _;
use ava_validators as _;
use ava_vm as _;

pub mod block;
pub mod error;
pub mod proposer;

pub use error::{Error, Result};

// Dev-dependencies are exercised only by the integration test crates under
// `tests/`; reference them here so the unit-test build of the lib does not warn
// about unused dev-deps.
#[cfg(test)]
mod dev_deps {
    use assert_matches as _;
    use hex as _;
    use pretty_assertions as _;
    use proptest as _;
    use serde as _;
    use serde_json as _;
}
