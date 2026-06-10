// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The fixed genesis chain list (specs 23 §3.5) and the well-known VM / fx IDs
//! (`utils/constants/vm_ids.go`, `vms/{secp256k1fx,nftfx,propertyfx}/factory.go`).
//!
//! X-Chain **first**, C-Chain **second** — never reorder; the `CreateChainTx`
//! IDs (and hence the X/C blockchain IDs the rest of the node uses) depend on
//! the order.

use ava_types::id::Id;

/// Builds the Go `ids.ID{'a','v','m', 0…}` style ascii-prefixed 32-byte id.
// const-evaluated: an overrun or overflow fails the build, not the runtime.
#[allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]
const fn ascii32(s: &str) -> [u8; 32] {
    let bytes = s.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < bytes.len() {
        out[i] = bytes[i];
        i += 1;
    }
    out
}

/// `constants.PlatformVMID`.
pub const PLATFORM_VM_ID_BYTES: [u8; 32] = ascii32("platformvm");
/// `constants.AVMID`.
pub const AVM_ID_BYTES: [u8; 32] = ascii32("avm");
/// `constants.EVMID`.
pub const EVM_ID_BYTES: [u8; 32] = ascii32("evm");
/// `secp256k1fx.ID`.
pub const SECP256K1FX_ID_BYTES: [u8; 32] = ascii32("secp256k1fx");
/// `nftfx.ID`.
pub const NFTFX_ID_BYTES: [u8; 32] = ascii32("nftfx");
/// `propertyfx.ID`.
pub const PROPERTYFX_ID_BYTES: [u8; 32] = ascii32("propertyfx");

/// `constants.PlatformVMID` as an [`Id`].
#[must_use]
pub fn platform_vm_id() -> Id {
    Id::from(PLATFORM_VM_ID_BYTES)
}

/// `constants.AVMID` as an [`Id`].
#[must_use]
pub fn avm_id() -> Id {
    Id::from(AVM_ID_BYTES)
}

/// `constants.EVMID` as an [`Id`].
#[must_use]
pub fn evm_id() -> Id {
    Id::from(EVM_ID_BYTES)
}

/// `secp256k1fx.ID` as an [`Id`].
#[must_use]
pub fn secp256k1fx_id() -> Id {
    Id::from(SECP256K1FX_ID_BYTES)
}

/// `nftfx.ID` as an [`Id`].
#[must_use]
pub fn nftfx_id() -> Id {
    Id::from(NFTFX_ID_BYTES)
}

/// `propertyfx.ID` as an [`Id`].
#[must_use]
pub fn propertyfx_id() -> Id {
    Id::from(PROPERTYFX_ID_BYTES)
}

/// One entry of the genesis chain list (`platformvm/genesis.Chain`).
#[derive(Clone, Debug)]
pub struct ChainSpec {
    /// The chain's genesis state bytes (`GenesisData`).
    pub genesis_data: Vec<u8>,
    /// The subnet validating the chain (`SubnetID`; Primary Network here).
    pub subnet_id: Id,
    /// The VM the chain runs (`VMID`).
    pub vm_id: Id,
    /// The fx IDs the chain supports (`FxIDs`).
    pub fx_ids: Vec<Id>,
    /// The human-readable chain name (`Name`).
    pub name: String,
}

/// The fixed genesis chain list: X-Chain first (avm + the three fxs), C-Chain
/// second (evm, no fxs) — specs 23 §3.5.
#[must_use]
pub fn genesis_chains(avm_genesis_bytes: Vec<u8>, c_chain_genesis: &str) -> Vec<ChainSpec> {
    vec![
        ChainSpec {
            genesis_data: avm_genesis_bytes,
            subnet_id: ava_types::constants::PRIMARY_NETWORK_ID,
            vm_id: avm_id(),
            fx_ids: vec![secp256k1fx_id(), nftfx_id(), propertyfx_id()],
            name: "X-Chain".to_string(),
        },
        ChainSpec {
            genesis_data: c_chain_genesis.as_bytes().to_vec(),
            subnet_id: ava_types::constants::PRIMARY_NETWORK_ID,
            vm_id: evm_id(),
            fx_ids: Vec::new(),
            name: "C-Chain".to_string(),
        },
    ]
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)] // fixed-size arrays/lists asserted by length
mod tests {
    use super::*;

    /// The ascii-prefixed IDs reproduce Go's `ids.ID{'a','v','m'}` layout
    /// (ascii prefix, zero-padded to 32 bytes).
    #[test]
    fn vm_ids_ascii_layout() {
        assert_eq!(&AVM_ID_BYTES[..3], b"avm");
        assert!(AVM_ID_BYTES[3..].iter().all(|&b| b == 0));
        assert_eq!(&EVM_ID_BYTES[..3], b"evm");
        assert_eq!(&PLATFORM_VM_ID_BYTES[..10], b"platformvm");
        assert_eq!(&SECP256K1FX_ID_BYTES[..11], b"secp256k1fx");
        assert_eq!(&NFTFX_ID_BYTES[..5], b"nftfx");
        assert_eq!(&PROPERTYFX_ID_BYTES[..10], b"propertyfx");
    }

    /// The genesis chain list is X-Chain first, C-Chain second, with the exact
    /// fx sets (specs 23 §3.5).
    #[test]
    fn chain_list_fixed_order() {
        let chains = genesis_chains(b"avm".to_vec(), "{}");
        assert_eq!(chains.len(), 2);
        assert_eq!(chains[0].name, "X-Chain");
        assert_eq!(chains[0].vm_id, avm_id());
        assert_eq!(
            chains[0].fx_ids,
            vec![secp256k1fx_id(), nftfx_id(), propertyfx_id()]
        );
        assert_eq!(chains[1].name, "C-Chain");
        assert_eq!(chains[1].vm_id, evm_id());
        assert!(chains[1].fx_ids.is_empty());
        assert_eq!(chains[1].genesis_data, b"{}");
    }
}
