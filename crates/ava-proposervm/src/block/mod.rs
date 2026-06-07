// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! ProposerVM block formats — byte-exact with Go `vms/proposervm/block`.
//!
//! - [`codec`] — registration order + interface (de)serialization + `parse`.
//! - [`stateless`] — the `serialize:"true"` bodies + `Epoch`.
//! - [`header`] — the signed-over `Header` (`BuildHeader`).
//! - [`option`] — the `option` block (`id = sha256(bytes)`).
//! - [`post_fork`] — `statelessBlock` (signed/unsigned) + `statelessGraniteBlock`.
//! - [`pre_fork`] — bare inner-block pass-through.

pub mod codec;
pub mod header;
pub(crate) mod hash;
pub mod option;
pub mod post_fork;
pub mod pre_fork;
pub mod stateless;

pub use codec::{
    CODEC_VERSION, ParsedBlock, TYPE_ID_GRANITE_BLOCK, TYPE_ID_OPTION, TYPE_ID_STATELESS_BLOCK,
    parse, parse_without_verification,
};
pub use header::Header;
pub use option::Option_;
pub use post_fork::{GraniteBlock, SignedBlock};
pub use pre_fork::PreForkBlock;
pub use stateless::{Epoch, StatelessUnsignedBlock, StatelessUnsignedGraniteBlock};
