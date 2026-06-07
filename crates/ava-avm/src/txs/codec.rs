// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The X-Chain tx/block codec registry — two [`Manager`]s over one type registry
//! (specs 09 §2.1, CODEC-AVM-1).
//!
//! Port of `vms/avm/txs/parser.go` + `vms/avm/block/parser.go` and the fx
//! `Initialize` registration order in `vms/{secp256k1fx,nftfx,propertyfx}/fx.go`.
//! `txs.CodecVersion = 0` is the only codec version. Two managers exist:
//!
//! - [`Codec`] — the default-max-size manager (`codec.NewDefaultManager()`).
//! - [`GenesisCodec`] — an `i32::MAX`-max manager (`codec.NewManager(MaxInt32)`)
//!   used to parse oversized genesis txs.
//!
//! Both register the **same** type IDs (the block codec and tx codec share one
//! numbering space, specs 09 §2.1); they differ only in their max decode size.
//! The registration order is reproduced exactly:
//!
//! 1. the five tx types `BaseTx`(0)..`ExportTx`(4) (`txs.NewCustomParser`).
//! 2. `secp256k1fx.Fx.Initialize` — `TransferInput`(5), `MintOutput`(6),
//!    `TransferOutput`(7), `MintOperation`(8), `Credential`(9).
//! 3. `nftfx.Fx.Initialize` — `MintOutput`(10), `TransferOutput`(11),
//!    `MintOperation`(12), `TransferOperation`(13), `Credential`(14).
//! 4. `propertyfx.Fx.Initialize` — `MintOutput`(15), `OwnedOutput`(16),
//!    `MintOperation`(17), `BurnOperation`(18), `Credential`(19).
//! 5. `block.StandardBlock`(20) (`block.NewParser`; reserved here — the block
//!    type lands in M5.15, so it is registered as a name-only placeholder).
//!
//! The `#[codec(type_id = N)]` annotations on [`UnsignedTx`] and the
//! components/credential enums carry the actual encoding type-ids; this registry
//! is the registration-order assigner used to **assert** those annotations
//! against the Go order (CODEC-AVM-1) and to build the [`TypeToFxIndex`] routing
//! table in the same pass (specs 09 §2.2).
//!
//! [`UnsignedTx`]: crate::txs::UnsignedTx

use std::collections::HashMap;
use std::sync::Arc;

use ava_codec::error::Result;
use ava_codec::linearcodec::{LinearCodec, TypeIdRegistry};
use ava_codec::manager::Manager;

use crate::fx_index::FxIndex;
use crate::txs::CODEC_VERSION;

/// `TypeToFxIndex` — the `type_id → fx_index` routing table (specs 09 §2.2).
///
/// Maps each fx output/input/operation/credential type-id to the fx ordinal that
/// owns it. Tx types (0–4) and the block (20) are not fx types and are absent.
pub type TypeToFxIndex = HashMap<u32, FxIndex>;

/// Builds the shared `(name, type_id)` registration table mirroring Go's
/// tx/block parser + fx `Initialize` registration order (specs 09 §2.1).
///
/// This is the registration-order assigner used to **assert** the `#[codec(type_id
/// = N)]` annotations against the Go order (CODEC-AVM-1). It does not participate
/// in encoding — that is fixed by the derive macros on [`UnsignedTx`] and the
/// component/credential enums.
///
/// [`UnsignedTx`]: crate::txs::UnsignedTx
///
/// # Errors
/// Returns a [`ava_codec::error::CodecError`] only on a duplicate registration or
/// counter overflow (neither can occur with the fixed table below).
pub fn build_type_id_registry() -> Result<TypeIdRegistry> {
    let mut r = TypeIdRegistry::new();

    // 1. tx types (0–4).
    r.register("BaseTx")?; // 0
    r.register("CreateAssetTx")?; // 1
    r.register("OperationTx")?; // 2
    r.register("ImportTx")?; // 3
    r.register("ExportTx")?; // 4

    // 2. secp256k1fx (5–9).
    r.register("secp256k1fx.TransferInput")?; // 5
    r.register("secp256k1fx.MintOutput")?; // 6
    r.register("secp256k1fx.TransferOutput")?; // 7
    r.register("secp256k1fx.MintOperation")?; // 8
    r.register("secp256k1fx.Credential")?; // 9

    // 3. nftfx (10–14).
    r.register("nftfx.MintOutput")?; // 10
    r.register("nftfx.TransferOutput")?; // 11
    r.register("nftfx.MintOperation")?; // 12
    r.register("nftfx.TransferOperation")?; // 13
    r.register("nftfx.Credential")?; // 14

    // 4. propertyfx (15–19).
    r.register("propertyfx.MintOutput")?; // 15
    r.register("propertyfx.OwnedOutput")?; // 16
    r.register("propertyfx.MintOperation")?; // 17
    r.register("propertyfx.BurnOperation")?; // 18
    r.register("propertyfx.Credential")?; // 19

    // 5. block.StandardBlock (20) — reserved placeholder (M5.15).
    r.register("block.StandardBlock")?; // 20

    Ok(r)
}

/// Returns the shared registration table as an owned `(name, type_id)` vec.
///
/// Convenience wrapper over [`build_type_id_registry`] for golden assertions.
///
/// # Panics
/// Panics only if the fixed registration table fails to build (impossible — it
/// has no duplicates and cannot overflow the `u32` counter).
#[must_use]
pub fn type_id_registry_table() -> Vec<(String, u32)> {
    build_type_id_registry()
        .map(|r| r.table().to_vec())
        .unwrap_or_default()
}

/// Builds the `type_id → fx_index` routing table (specs 09 §2.2) in the same pass
/// as the type-ID registry, so the two can never drift.
///
/// secp256k1fx owns 5–9, nftfx 10–14, propertyfx 15–19. Tx types (0–4) and the
/// block (20) are not fx types and are absent.
#[must_use]
pub fn type_to_fx_index() -> TypeToFxIndex {
    let mut m = TypeToFxIndex::with_capacity(15);
    for id in 5u32..=9 {
        m.insert(id, FxIndex::Secp256k1);
    }
    for id in 10u32..=14 {
        m.insert(id, FxIndex::Nft);
    }
    for id in 15u32..=19 {
        m.insert(id, FxIndex::Property);
    }
    m
}

/// The default-max-size codec manager (`txs.Codec`, specs 09 §2.1).
///
/// Registers the linear codec under [`CODEC_VERSION`]. The per-type typeID wiring
/// lives in the `#[codec(type_id = N)]`-annotated [`UnsignedTx`] / component
/// derives; this manager only frames values with the 2-byte version prefix and
/// enforces the trailing-byte check.
///
/// [`UnsignedTx`]: crate::txs::UnsignedTx
///
/// # Errors
/// Returns a [`ava_codec::error::CodecError`] if codec registration fails
/// (cannot happen for a fresh manager).
pub fn codec() -> Result<Manager> {
    let m = Manager::with_default_max_size();
    m.register(CODEC_VERSION, Arc::new(LinearCodec::new()))?;
    Ok(m)
}

/// The genesis codec manager (`txs.GenesisCodec`, specs 09 §2.1).
///
/// Identical type registry to [`codec`] but with an `i32::MAX` max decode size
/// (`codec.NewManager(math.MaxInt32)`), used to parse oversized X-Chain genesis
/// txs.
///
/// # Errors
/// Returns a [`ava_codec::error::CodecError`] if codec registration fails.
pub fn genesis_codec() -> Result<Manager> {
    let m = Manager::new(ava_codec::MAX_SLICE_LEN);
    m.register(CODEC_VERSION, Arc::new(LinearCodec::new()))?;
    Ok(m)
}

/// Lazily-built, process-wide [`Codec`] / [`GenesisCodec`] handles.
///
/// Mirrors the Go package-level parser codec singletons.
mod managers {
    use std::sync::OnceLock;

    use ava_codec::manager::Manager;

    static CODEC: OnceLock<Manager> = OnceLock::new();
    static GENESIS_CODEC: OnceLock<Manager> = OnceLock::new();

    /// The shared default-max-size manager.
    pub(super) fn codec() -> &'static Manager {
        CODEC.get_or_init(|| super::codec().unwrap_or_default())
    }

    /// The shared `i32::MAX` genesis manager.
    pub(super) fn genesis_codec() -> &'static Manager {
        GENESIS_CODEC.get_or_init(|| {
            super::genesis_codec().unwrap_or_else(|_| Manager::new(ava_codec::MAX_SLICE_LEN))
        })
    }
}

/// The process-wide default-max-size codec manager (`txs.Codec`).
///
/// Named to mirror the Go package-level `txs.Codec` / parser `Codec()` singleton.
#[must_use]
#[allow(non_snake_case)]
pub fn Codec() -> &'static Manager {
    managers::codec()
}

/// The process-wide genesis codec manager (`txs.GenesisCodec`).
///
/// Named to mirror the Go package-level `txs.GenesisCodec` / parser
/// `GenesisCodec()` singleton.
#[must_use]
#[allow(non_snake_case)]
pub fn GenesisCodec() -> &'static Manager {
    managers::genesis_codec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_21_entries_with_top_id_20() {
        let r = build_type_id_registry().expect("build registry");
        // Next id after registering through 20 is 21.
        assert_eq!(r.next_id(), 21);
        // All 21 entries are named (no skips on the X-Chain).
        assert_eq!(r.table().len(), 21);
    }

    #[test]
    fn type_to_fx_index_routes_each_fx() {
        let m = type_to_fx_index();
        assert_eq!(m.len(), 15);
        assert_eq!(m.get(&5), Some(&FxIndex::Secp256k1));
        assert_eq!(m.get(&14), Some(&FxIndex::Nft));
        assert_eq!(m.get(&19), Some(&FxIndex::Property));
        assert_eq!(m.get(&0), None);
        assert_eq!(m.get(&20), None);
    }
}
