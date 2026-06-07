// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The P-Chain tx/block codec registry — two [`Manager`]s over one type registry
//! (specs 08 §2.1).
//!
//! Port of `vms/platformvm/txs/codec.go`. `txs.CodecVersion = 0` is the only
//! codec version. Two managers exist:
//!
//! - [`Codec`] — the default-max-size manager used for ordinary txs/blocks.
//! - [`GenesisCodec`] — an `i32::MAX`-max-slice manager used for oversized
//!   genesis txs and L1-validator value marshalling.
//!
//! Both register the **same** type IDs (the block codec and tx codec share one
//! numbering space, specs 08 §2.1); they differ only in their max decode size.
//! The registration order is reproduced exactly:
//!
//! 1. `SkipRegistrations(5)` — reserve block IDs 0–4 (the Apricot blocks).
//! 2. secp256k1fx at 5–11, with the `SkipRegistrations(1)` `MintInput`(6) /
//!    `MintOutput`(8) gaps the AVM fills (do **not** collapse).
//! 3. the Apricot tx types (12–20) + `stakeable.LockIn`/`LockOut` (21–22).
//! 4. the Banff tx types (23–26) + `signer.Empty`/`ProofOfPossession` (27–28).
//! 5. `SkipRegistrations(4)` — reserve Banff block IDs 29–32.
//! 6. Durango (33–34), Etna (35–39), Helicon (40–42) tx types.

use std::sync::Arc;

use ava_codec::error::Result;
use ava_codec::linearcodec::{LinearCodec, TypeIdRegistry};
use ava_codec::manager::Manager;

use crate::CODEC_VERSION;

/// Builds the shared `(name, type_id)` registration table mirroring Go's
/// `txs/codec.go` `init()` order (and the block codec's shared numbering space).
///
/// This is the registration-order assigner used to **assert** the `#[codec(type_id
/// = N)]` annotations against the Go order (the golden type_id table). It does not
/// participate in encoding — that is fixed by the derive macro on [`UnsignedTx`].
///
/// [`UnsignedTx`]: crate::txs::UnsignedTx
///
/// # Errors
/// Returns a [`ava_codec::error::CodecError`] only on a duplicate registration or
/// counter overflow (neither can occur with the fixed table below).
pub fn build_type_id_registry() -> Result<TypeIdRegistry> {
    let mut r = TypeIdRegistry::new();

    // 1. Reserve the 5 Apricot block IDs (0–4).
    r.skip_registrations(5)?;

    // 2. secp256k1fx (5–11), with the MintInput(6)/MintOutput(8) gaps.
    r.register("secp256k1fx.TransferInput")?; // 5
    r.skip_registrations(1)?; // 6  (MintInput — AVM only)
    r.register("secp256k1fx.TransferOutput")?; // 7
    r.skip_registrations(1)?; // 8  (MintOutput — AVM only)
    r.register("secp256k1fx.Credential")?; // 9
    r.register("secp256k1fx.Input")?; // 10
    r.register("secp256k1fx.OutputOwners")?; // 11

    // 3. Apricot tx types (12–20) + stakeable (21–22).
    r.register("AddValidatorTx")?; // 12
    r.register("AddSubnetValidatorTx")?; // 13
    r.register("AddDelegatorTx")?; // 14
    r.register("CreateChainTx")?; // 15
    r.register("CreateSubnetTx")?; // 16
    r.register("ImportTx")?; // 17
    r.register("ExportTx")?; // 18
    r.register("AdvanceTimeTx")?; // 19
    r.register("RewardValidatorTx")?; // 20
    r.register("stakeable.LockIn")?; // 21
    r.register("stakeable.LockOut")?; // 22

    // 4. Banff tx types (23–26) + signer (27–28).
    r.register("RemoveSubnetValidatorTx")?; // 23
    r.register("TransformSubnetTx")?; // 24
    r.register("AddPermissionlessValidatorTx")?; // 25
    r.register("AddPermissionlessDelegatorTx")?; // 26
    r.register("signer.Empty")?; // 27
    r.register("signer.ProofOfPossession")?; // 28

    // 5. Reserve the 4 Banff block IDs (29–32).
    r.skip_registrations(4)?;

    // 6. Durango (33–34), Etna (35–39), Helicon (40–42).
    r.register("TransferSubnetOwnershipTx")?; // 33
    r.register("BaseTx")?; // 34
    r.register("ConvertSubnetToL1Tx")?; // 35
    r.register("RegisterL1ValidatorTx")?; // 36
    r.register("SetL1ValidatorWeightTx")?; // 37
    r.register("IncreaseL1ValidatorBalanceTx")?; // 38
    r.register("DisableL1ValidatorTx")?; // 39
    r.register("AddAutoRenewedValidatorTx")?; // 40
    r.register("SetAutoRenewedValidatorConfigTx")?; // 41
    r.register("RewardAutoRenewedValidatorTx")?; // 42

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

/// The default-max-size codec manager (`txs.Codec`, specs 08 §2.1).
///
/// Registers the linear codec under [`CODEC_VERSION`]. The per-type typeID wiring
/// lives in the `#[codec(type_id = N)]`-annotated [`UnsignedTx`] / `Tx` /
/// `Block` derives; this manager only frames values with the 2-byte version
/// prefix and enforces the trailing-byte check.
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

/// The genesis codec manager (`txs.GenesisCodec`, specs 08 §2.1).
///
/// Identical type registry to [`codec`] but with an `i32::MAX` max decode size,
/// used to parse oversized P-Chain genesis txs and to marshal L1-validator
/// values.
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
/// Mirrors the Go package-level `txs.Codec` / `txs.GenesisCodec` singletons.
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
/// Named to mirror the Go package-level `txs.Codec` singleton.
#[must_use]
#[allow(non_snake_case)]
pub fn Codec() -> &'static Manager {
    managers::codec()
}

/// The process-wide genesis codec manager (`txs.GenesisCodec`).
///
/// Named to mirror the Go package-level `txs.GenesisCodec` singleton.
#[must_use]
#[allow(non_snake_case)]
pub fn GenesisCodec() -> &'static Manager {
    managers::genesis_codec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_43_entries_with_top_id_42() {
        let r = build_type_id_registry().expect("build registry");
        // Next id after registering through 42 is 43.
        assert_eq!(r.next_id(), 43);
        // 9 secp/stakeable/signer + 23 tx types = the 32 named registrations.
        assert_eq!(r.table().len(), 32);
    }
}
