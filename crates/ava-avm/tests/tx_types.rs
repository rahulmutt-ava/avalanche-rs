// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M5.2 TDD entry point — the X-Chain (AVM) tx model types (specs/09 §3).
//!
//! Exercises the [`UnsignedTx`] variant set + embedded-`BaseTx` accessor, and the
//! `serialize:"false"` posture of [`FxCredential::fx_id`].

#![allow(unused_crate_dependencies)]

use ava_avm::txs::components::AvaxBaseTx;
use ava_avm::txs::{
    BaseTx, CreateAssetTx, ExportTx, FxCredential, ImportTx, OperationTx, UnsignedTx,
};
use ava_secp256k1fx::Credential;
use ava_types::id::Id;

/// Every `UnsignedTx` variant constructs with minimal fields and exposes the
/// embedded `avax.BaseTx` via [`UnsignedTx::base`].
#[test]
fn unsigned_tx_enum_variants() {
    let base = BaseTx::default();

    let variants = [
        UnsignedTx::Base(base.clone()),
        UnsignedTx::CreateAsset(CreateAssetTx {
            base: base.clone(),
            name: "Asset".to_string(),
            symbol: "AST".to_string(),
            denomination: 0,
            states: Vec::new(),
        }),
        UnsignedTx::Operation(OperationTx {
            base: base.clone(),
            ops: Vec::new(),
        }),
        UnsignedTx::Import(ImportTx {
            base: base.clone(),
            source_chain: Id::EMPTY,
            imported_ins: Vec::new(),
        }),
        UnsignedTx::Export(ExportTx {
            base: base.clone(),
            destination_chain: Id::EMPTY,
            exported_outs: Vec::new(),
        }),
    ];

    for v in &variants {
        // Every X-Chain tx embeds an `avax.BaseTx`; the accessor returns it.
        let embedded: &AvaxBaseTx = v.base();
        assert_eq!(embedded, &base.base);
    }
}

/// `FxCredential` exposes `fx_id` (runtime routing) but it MUST NOT appear on the
/// wire (Go `serialize:"false"`). Structural check: the marshaled bytes of an
/// `FxCredential` whose `fx_id` is set to all-`0xAB` do not contain that 32-byte
/// run.
#[test]
fn fx_credential_fx_id_not_serialized() {
    use ava_codec::Serializable;

    let fx_id = Id::from([0xABu8; 32]);
    let cred = FxCredential::new(fx_id, Credential::new(vec![[0x11u8; 65]]));

    // The accessor exposes the routing fx_id.
    assert_eq!(cred.fx_id(), fx_id);

    let mut p = ava_codec::packer::Packer::with_max_size(usize::MAX);
    cred.marshal_into(&mut p);
    let bytes = p.into_bytes();

    let needle = [0xABu8; 32];
    let contains = bytes.windows(needle.len()).any(|w| w == needle);
    assert!(
        !contains,
        "fx_id must not be serialized (serialize:\"false\")"
    );
}
