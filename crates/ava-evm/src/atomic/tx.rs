// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Atomic X<->C transaction types + byte-exact linear codec (spec 10 §6.1/§6.2).
//!
//! Port of coreth `plugin/evm/atomic/{tx,import_tx,export_tx,codec,params}.go`.
//! The atomic tx codec is the **avalanchego linear codec** (NOT RLP): the same
//! `#[derive(AvaCodec)]` stack the X/P chains use. The C-Chain reuses the
//! byte-exact, codec-serializable avax/fx components from the X-Chain
//! (`ava_avm::txs::components` / `ava_avm::txs::credential`) because the atomic
//! codec's secp fx type-ids (`TransferInput`=5, `TransferOutput`=7,
//! `Credential`=9) coincide with the X-Chain codec's.
//!
//! ## Atomic codec type-id registry (coreth `atomic/codec.go` `init`)
//!
//! Distinct from the X-Chain codec — the import/export tx types take ids 0/1:
//!
//! | id | type |
//! |----|------|
//! | 0  | `UnsignedImportTx` |
//! | 1  | `UnsignedExportTx` |
//! | 2–4 | (`SkipRegistrations(3)`) |
//! | 5  | `secp256k1fx.TransferInput` |
//! | 6  | (`SkipRegistrations(1)`) |
//! | 7  | `secp256k1fx.TransferOutput` |
//! | 8  | (`SkipRegistrations(1)`) |
//! | 9  | `secp256k1fx.Credential` |
//! | 10 | `secp256k1fx.Input` |
//! | 11 | `secp256k1fx.OutputOwners` |
//!
//! Only ids 0/1/5/7/9 appear on the wire for the Import/Export round-trip; 5/7/9
//! match the X-Chain numbering, so the reused X-Chain component encodings are
//! byte-identical (verified against Go-executed golden vectors — see
//! `tests/cchain_atomic_tx.rs`).

use std::sync::Arc;

use ava_avm::txs::components::{Output, TransferableInput, TransferableOutput};
use ava_avm::txs::credential::FxCredential;
use ava_codec::AvaCodec;
use ava_codec::error::Result as CodecResult;
use ava_codec::linearcodec::LinearCodec;
use ava_codec::manager::Manager;
use ava_crypto::hashing;
use ava_types::id::Id;
use ava_vm::components::avax::shared_memory::{Element, Requests};

/// `atomic.CodecVersion` — the only atomic codec version (coreth
/// `atomic/codec.go:16`).
pub const CODEC_VERSION: u16 = 0;

// ---------------------------------------------------------------------------
// Gas / conversion constants (coreth `plugin/evm/atomic/tx.go`,`params.go`)
// ---------------------------------------------------------------------------

/// `X2CRateUint64` — the nAVAX→wei conversion rate (1 nAVAX = 1 gWei = 1e9 wei).
///
/// coreth `plugin/evm/atomic/tx.go:33` (`X2CRateUint64 uint64 = 1_000_000_000`).
pub const X2C_RATE: u64 = 1_000_000_000;

/// `TxBytesGas` — gas charged per atomic-tx byte.
///
/// coreth `plugin/evm/atomic/tx.go:52` (`TxBytesGas uint64 = 1`).
pub const TX_BYTES_GAS: u64 = 1;

/// `secp256k1fx.CostPerSignature` — gas charged per signature.
///
/// avalanchego `vms/secp256k1fx/input.go:14` (`CostPerSignature uint64 = 1000`).
pub const COST_PER_SIGNATURE: u64 = 1000;

/// `EVMOutputGas` — gas for one `EVMOutput`:
/// `(AddressLength + LongLen + HashLen) * TxBytesGas = (20 + 8 + 32) * 1 = 60`.
///
/// coreth `plugin/evm/atomic/tx.go:53`.
pub const EVM_OUTPUT_GAS: u64 = (20 + 8 + 32) * TX_BYTES_GAS;

/// `EVMInputGas` — gas for one `EVMInput`:
/// `(AddressLength + LongLen + HashLen + LongLen) * TxBytesGas + CostPerSignature`
/// `= (20 + 8 + 32 + 8) * 1 + 1000 = 1068`.
///
/// coreth `plugin/evm/atomic/tx.go:54`.
pub const EVM_INPUT_GAS: u64 = (20 + 8 + 32 + 8) * TX_BYTES_GAS + COST_PER_SIGNATURE;

// ---------------------------------------------------------------------------
// EVMOutput / EVMInput (coreth `plugin/evm/atomic/tx.go:64`,`:79`)
// ---------------------------------------------------------------------------

/// `atomic.EVMOutput` — an output that credits the EVM state on import.
///
/// Field order = serialization order (coreth `tx.go:64`): `address` (20 raw
/// bytes, no length prefix — `common.Address`), `amount` (`u64`), `asset_id`
/// (`ids.ID`, 32 raw bytes).
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct EvmOutput {
    /// `Address` — the 20-byte EVM account credited (`common.Address`).
    #[codec]
    pub address: [u8; 20],
    /// `Amount` — the amount credited (in the asset's native denomination).
    #[codec]
    pub amount: u64,
    /// `AssetID` — the asset this output is denominated in.
    #[codec]
    pub asset_id: Id,
}

/// `atomic.EVMInput` — an input that debits the EVM state on export.
///
/// Field order = serialization order (coreth `tx.go:79`): `address`, `amount`,
/// `asset_id`, `nonce`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct EvmInput {
    /// `Address` — the 20-byte EVM account debited (`common.Address`).
    #[codec]
    pub address: [u8; 20],
    /// `Amount` — the amount debited.
    #[codec]
    pub amount: u64,
    /// `AssetID` — the asset this input is denominated in.
    #[codec]
    pub asset_id: Id,
    /// `Nonce` — the spending account's nonce (replay protection on export).
    #[codec]
    pub nonce: u64,
}

impl EvmOutput {
    /// The 32-byte shared-memory trait key Go uses for this output's address:
    /// the raw `common.Address` bytes are not the trait; outputs do not produce
    /// traits (import consumes, it does not put). Provided for symmetry only.
    #[must_use]
    pub fn address(&self) -> [u8; 20] {
        self.address
    }
}

// ---------------------------------------------------------------------------
// UnsignedImportTx (coreth `plugin/evm/atomic/import_tx.go:48`)
// ---------------------------------------------------------------------------

/// `atomic.UnsignedImportTx` — imports UTXOs from `source_chain` and credits the
/// EVM via `outs`.
///
/// The embedded `Metadata` (coreth) carries only non-serialized cache fields
/// (`id`/`unsignedBytes`/`bytes`), so the wire format starts at `network_id`.
/// Field order = serialization order (coreth `import_tx.go:48`): `network_id`,
/// `blockchain_id`, `source_chain`, `imported_inputs`, `outs`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct UnsignedImportTx {
    /// `NetworkID` — the network this tx was issued on.
    #[codec]
    pub network_id: u32,
    /// `BlockchainID` — this blockchain's id (the C-Chain).
    #[codec]
    pub blockchain_id: Id,
    /// `SourceChain` — the chain whose UTXOs are consumed.
    #[codec]
    pub source_chain: Id,
    /// `ImportedInputs` — the consumed atomic inputs (from shared memory).
    #[codec]
    pub imported_inputs: Vec<TransferableInput>,
    /// `Outs` — the EVM outputs credited.
    #[codec]
    pub outs: Vec<EvmOutput>,
}

impl UnsignedImportTx {
    /// `(*UnsignedImportTx).AtomicOps` (coreth `import_tx.go:192`).
    ///
    /// Returns `(source_chain, Requests{ remove: [in.InputID()..] })`: the
    /// imported UTXOs are removed from the source chain's shared-memory half.
    #[must_use]
    pub fn atomic_ops(&self) -> (Id, Requests) {
        let remove = self
            .imported_inputs
            .iter()
            .map(|input| input.input_id().to_bytes().to_vec())
            .collect();
        (
            self.source_chain,
            Requests {
                remove,
                put: Vec::new(),
            },
        )
    }
}

// ---------------------------------------------------------------------------
// UnsignedExportTx (coreth `plugin/evm/atomic/export_tx.go:45`)
// ---------------------------------------------------------------------------

/// `atomic.UnsignedExportTx` — debits the EVM via `ins` and exports UTXOs to
/// `destination_chain`.
///
/// Field order = serialization order (coreth `export_tx.go:45`): `network_id`,
/// `blockchain_id`, `destination_chain`, `ins`, `exported_outputs`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct UnsignedExportTx {
    /// `NetworkID` — the network this tx was issued on.
    #[codec]
    pub network_id: u32,
    /// `BlockchainID` — this blockchain's id (the C-Chain).
    #[codec]
    pub blockchain_id: Id,
    /// `DestinationChain` — the chain the exported UTXOs are sent to.
    #[codec]
    pub destination_chain: Id,
    /// `Ins` — the EVM inputs debited.
    #[codec]
    pub ins: Vec<EvmInput>,
    /// `ExportedOutputs` — the produced atomic outputs (to shared memory).
    #[codec]
    pub exported_outputs: Vec<TransferableOutput>,
}

impl UnsignedExportTx {
    /// `(*UnsignedExportTx).AtomicOps` (coreth `export_tx.go:185`).
    ///
    /// Returns `(destination_chain, Requests{ put: [Element{key, value, traits}..] })`
    /// where each element is the exported UTXO: `key = utxo.InputID()`,
    /// `value = Codec.Marshal(0, utxo)`, `traits = out.Addresses()`. The UTXO's
    /// `TxID` is the signed-tx id; its `OutputIndex` is `i` (0-based over the
    /// exported outputs).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if a UTXO fails to marshal.
    pub fn atomic_ops(&self, tx_id: Id) -> CodecResult<(Id, Requests)> {
        let mut put = Vec::with_capacity(self.exported_outputs.len());
        for (i, out) in self.exported_outputs.iter().enumerate() {
            let output_index =
                u32::try_from(i).map_err(|_| ava_codec::error::CodecError::MaxSliceLenExceeded)?;
            let utxo = ExportUtxo {
                tx_id,
                output_index,
                asset_id: out.asset_id,
                out: out.out.clone(),
            };
            let key = utxo.input_id().to_bytes().to_vec();
            let value = codec().marshal(CODEC_VERSION, &utxo)?;
            let traits = output_addresses(&out.out);
            put.push(Element { key, value, traits });
        }
        Ok((
            self.destination_chain,
            Requests {
                remove: Vec::new(),
                put,
            },
        ))
    }
}

/// `avax.UTXO` — the codec-serializable shared-memory value an export produces
/// (`vms/components/avax/utxo.go`). Byte-identical to the X-Chain UTXO
/// (`ava_avm` `Utxo`) because the atomic codec's `TransferOutput` type-id (7)
/// matches the X-Chain's. Local copy so the encoding goes through the *atomic*
/// codec, not the X-Chain singleton.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
struct ExportUtxo {
    #[codec]
    tx_id: Id,
    #[codec]
    output_index: u32,
    #[codec]
    asset_id: Id,
    #[codec]
    out: Output,
}

impl ExportUtxo {
    /// `InputID()` — `tx_id.prefix(output_index)`.
    fn input_id(&self) -> Id {
        self.tx_id.prefix(&[u64::from(self.output_index)])
    }
}

/// `utxo.Out.Addresses()` — the owner addresses of an fx output as raw bytes,
/// used as the shared-memory `Element` traits (coreth `export_tx.go:208`).
fn output_addresses(out: &Output) -> Vec<Vec<u8>> {
    let addrs = match out {
        Output::SecpTransfer(o) => &o.owners.addrs,
        Output::SecpMint(o) => &o.owners.addrs,
    };
    addrs.iter().map(|a| a.to_bytes().to_vec()).collect()
}

// ---------------------------------------------------------------------------
// AtomicTx interface enum + signed Tx envelope (coreth `atomic/tx.go:175`)
// ---------------------------------------------------------------------------

/// `atomic.UnsignedAtomicTx` — the interface enum registered into the atomic
/// codec (coreth `atomic/codec.go`: `UnsignedImportTx`=0, `UnsignedExportTx`=1).
///
/// Marshals the `u32` type-id then the concrete payload (the interface framing
/// the signed [`Tx`] envelope and shared-memory carry).
#[derive(AvaCodec, Clone, Debug, PartialEq, Eq)]
#[codec(type_registry)]
pub enum AtomicTx {
    /// `UnsignedImportTx` (type_id 0).
    #[codec(type_id = 0)]
    Import(UnsignedImportTx),
    /// `UnsignedExportTx` (type_id 1).
    #[codec(type_id = 1)]
    Export(UnsignedExportTx),
}

impl Default for AtomicTx {
    fn default() -> Self {
        AtomicTx::Import(UnsignedImportTx::default())
    }
}

impl AtomicTx {
    /// `(UnsignedAtomicTx).AtomicOps` — dispatch to the concrete tx.
    ///
    /// `tx_id` is the signed-tx id; it is only consumed by the Export arm (the
    /// exported UTXO's `TxID`), but is required for parity with Go's signature.
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if an export UTXO fails to
    /// marshal.
    pub fn atomic_ops(&self, tx_id: Id) -> CodecResult<(Id, Requests)> {
        match self {
            AtomicTx::Import(tx) => Ok(tx.atomic_ops()),
            AtomicTx::Export(tx) => tx.atomic_ops(tx_id),
        }
    }
}

/// `atomic.Tx` — a signed atomic transaction (coreth `atomic/tx.go:175`).
///
/// Wire layout (codec v0): the typeid-prefixed `unsigned` body then the
/// credentials `creds`. `tx_id`/`bytes` are derived caches populated by
/// [`Tx::initialize`] / [`Tx::parse`] and are **not** on the wire (no `#[codec]`
/// tag), mirroring the embedded `Metadata`.
#[derive(AvaCodec, Clone, Debug, Default, PartialEq, Eq)]
pub struct Tx {
    /// The transaction body (interface → typeid-prefixed).
    #[codec]
    pub unsigned: AtomicTx,
    /// The fx credentials (each interface → typeid-prefixed).
    #[codec]
    pub creds: Vec<FxCredential>,
    /// `= sha256(signed_bytes)`. Not serialized (coreth `Metadata.id`).
    pub tx_id: Id,
    /// Cached signed bytes. Not serialized (coreth `Metadata.bytes`).
    pub bytes: Vec<u8>,
}

impl Tx {
    /// Builds an unsigned-only [`Tx`] (no credentials attached yet).
    #[must_use]
    pub fn new(unsigned: AtomicTx) -> Self {
        Self {
            unsigned,
            creds: Vec::new(),
            tx_id: Id::EMPTY,
            bytes: Vec::new(),
        }
    }

    /// `Tx.Initialize` — marshals the whole tx, then derives the cached signed
    /// bytes and `tx_id = sha256(signed_bytes)` (coreth `metadata.go:18`).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if marshalling fails.
    pub fn initialize(&mut self) -> CodecResult<()> {
        let signed_bytes = codec().marshal(CODEC_VERSION, self)?;
        self.set_bytes(signed_bytes);
        Ok(())
    }

    fn set_bytes(&mut self, signed_bytes: Vec<u8>) {
        self.tx_id = Id::from(hashing::sha256(&signed_bytes));
        self.bytes = signed_bytes;
    }

    /// `atomic.ExtractAtomicTx` — decodes a signed tx and reproduces the cached
    /// signed bytes + `tx_id` (coreth `codec.go:80`).
    ///
    /// # Errors
    /// Returns a [`ava_codec::error::CodecError`] if the bytes fail to decode.
    pub fn parse(signed_bytes: &[u8]) -> CodecResult<Self> {
        let mut tx = Tx::default();
        codec().unmarshal(signed_bytes, &mut tx)?;
        tx.set_bytes(signed_bytes.to_vec());
        Ok(tx)
    }

    /// The tx id (`sha256(signed_bytes)`; `Id::EMPTY` until initialized).
    #[must_use]
    pub fn id(&self) -> Id {
        self.tx_id
    }

    /// The cached signed bytes (empty until [`Tx::initialize`]/[`Tx::parse`]).
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

// ---------------------------------------------------------------------------
// Atomic codec manager (coreth `atomic/codec.go` `init`)
// ---------------------------------------------------------------------------

/// The process-wide atomic codec manager (`atomic.Codec`).
///
/// A default-max-size [`Manager`] with one [`LinearCodec`] registered at
/// [`CODEC_VERSION`]. The per-type type-ids are baked into the
/// `#[derive(AvaCodec)]` impls on [`AtomicTx`] / the reused X-Chain components,
/// so this manager only frames values with the 2-byte version prefix and
/// enforces the trailing-byte check (matching `codec.NewDefaultManager`).
#[must_use]
pub fn codec() -> &'static Manager {
    use std::sync::OnceLock;
    static CODEC: OnceLock<Manager> = OnceLock::new();
    CODEC.get_or_init(|| {
        let m = Manager::with_default_max_size();
        // Registration cannot fail for a fresh manager; fall back to a bare
        // manager so the accessor stays infallible (mirrors ava-avm's pattern).
        let _ = m.register(CODEC_VERSION, Arc::new(LinearCodec::new()));
        m
    })
}

#[cfg(test)]
mod tests {
    use ava_avm::txs::components::{Input as FxInput, Output as FxOutput};
    use ava_secp256k1fx::{OutputOwners, TransferInput, TransferOutput};
    use ava_types::short_id::ShortId;

    use super::*;

    /// The Go-golden exported-UTXO codec value (the `value` of the single export
    /// `Element` in `tests/vectors/cchain/atomic/atomic_txs.json`).
    const EXPORT_UTXO_VALUE_HEX: &str = "000006ceeed2e0b93c5cb22055711767ce439ce220c94297136f64dd54438cd4fddc00000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa000000070000000000000bb8000000000000000000000001000000010505050505050505050505050505050505050505";

    /// Go-golden bare-struct (no type_id) hex of `EvmOutput{0x01×20, 1000, 0xAA}`.
    const EVM_OUTPUT_HEX: &str = "0000010101010101010101010101010101010101010100000000000003e8aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    /// Go-golden hex of `EvmInput{0x02×20, 2000, 0xAA, nonce=7}`.
    const EVM_INPUT_HEX: &str = "0000020202020202020202020202020202020202020200000000000007d0aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa0000000000000007";
    /// Go-golden bare-struct hex of the golden `UnsignedImportTx`.
    const IMPORT_STRUCT_HEX: &str = "0000000000011111111111111111111111111111111111111111111111111111111111111111222222222222222222222222222222222222222222222222222222222222222200000001444444444444444444444444444444444444444444444444444444444444444400000001aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa00000005000000000000138800000001000000000000000101010101010101010101010101010101010101010000000000001387aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    /// Go-golden bare-struct hex of the golden `UnsignedExportTx`.
    const EXPORT_STRUCT_HEX: &str = "000000000001111111111111111111111111111111111111111111111111111111111111111133333333333333333333333333333333333333333333333333333333333333330000000102020202020202020202020202020202020202020000000000000bb8aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa000000000000000700000001aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa000000070000000000000bb8000000000000000000000001000000010505050505050505050505050505050505050505";

    /// 32-byte id with every byte = `b`.
    fn id32(b: u8) -> Id {
        Id::from([b; 32])
    }

    /// The deterministic AVAX asset id used by the Go golden dump (0xAA × 32).
    fn avax_asset() -> Id {
        id32(0xAA)
    }

    /// The Go-golden import tx (matches `tests/vectors/cchain/atomic/atomic_txs.json`).
    fn golden_import() -> UnsignedImportTx {
        UnsignedImportTx {
            network_id: 1,
            blockchain_id: id32(0x11),
            source_chain: id32(0x22),
            imported_inputs: vec![TransferableInput {
                tx_id: id32(0x44),
                output_index: 1,
                asset_id: avax_asset(),
                r#in: FxInput::SecpTransfer(TransferInput::new(5000, vec![0])),
            }],
            outs: vec![EvmOutput {
                address: [0x01; 20],
                amount: 4999,
                asset_id: avax_asset(),
            }],
        }
    }

    /// The Go-golden export tx (matches the JSON golden vectors).
    fn golden_export() -> UnsignedExportTx {
        UnsignedExportTx {
            network_id: 1,
            blockchain_id: id32(0x11),
            destination_chain: id32(0x33),
            ins: vec![EvmInput {
                address: [0x02; 20],
                amount: 3000,
                asset_id: avax_asset(),
                nonce: 7,
            }],
            exported_outputs: vec![TransferableOutput {
                asset_id: avax_asset(),
                out: FxOutput::SecpTransfer(TransferOutput {
                    amt: 3000,
                    owners: OutputOwners {
                        locktime: 0,
                        threshold: 1,
                        addrs: vec![ShortId::from([0x05; 20])],
                    },
                }),
            }],
        }
    }

    #[test]
    fn constants_match_go() {
        // Go-executed golden dump (see tests/vectors/cchain/atomic/_provenance.md).
        assert_eq!(X2C_RATE, 1_000_000_000);
        assert_eq!(TX_BYTES_GAS, 1);
        assert_eq!(COST_PER_SIGNATURE, 1000);
        assert_eq!(EVM_OUTPUT_GAS, 60);
        assert_eq!(EVM_INPUT_GAS, 1068);
    }

    #[test]
    fn import_export_serialize_byte_exact() {
        let marshal = |v: &dyn ava_codec::Serializable| {
            hex::encode(codec().marshal(CODEC_VERSION, v).expect("marshal"))
        };

        // EvmOutput / EvmInput (bare struct, no type prefix).
        let out = EvmOutput {
            address: [0x01; 20],
            amount: 1000,
            asset_id: avax_asset(),
        };
        assert_eq!(marshal(&out), EVM_OUTPUT_HEX);
        let evm_in = EvmInput {
            address: [0x02; 20],
            amount: 2000,
            asset_id: avax_asset(),
            nonce: 7,
        };
        assert_eq!(marshal(&evm_in), EVM_INPUT_HEX);

        // UnsignedImportTx / UnsignedExportTx (bare struct, no interface type_id).
        assert_eq!(marshal(&golden_import()), IMPORT_STRUCT_HEX);
        assert_eq!(marshal(&golden_export()), EXPORT_STRUCT_HEX);

        // The interface (AtomicTx) form inserts the u32 type_id (0/1) after the
        // 2-byte version prefix, then round-trips.
        let import = AtomicTx::Import(golden_import());
        let import_hex = marshal(&import);
        assert_eq!(&import_hex[..4], &IMPORT_STRUCT_HEX[..4]); // shared version
        assert_eq!(&import_hex[4..12], "00000000"); // type_id 0
        assert_eq!(&import_hex[12..], &IMPORT_STRUCT_HEX[4..]); // shared body
        let mut decoded = AtomicTx::default();
        codec()
            .unmarshal(
                &codec().marshal(CODEC_VERSION, &import).expect("m"),
                &mut decoded,
            )
            .expect("import round-trip");
        assert_eq!(decoded, import);

        let export = AtomicTx::Export(golden_export());
        let export_hex = marshal(&export);
        assert_eq!(&export_hex[4..12], "00000001"); // type_id 1
        assert_eq!(&export_hex[12..], &EXPORT_STRUCT_HEX[4..]);
        let mut decoded = AtomicTx::default();
        codec()
            .unmarshal(
                &codec().marshal(CODEC_VERSION, &export).expect("m"),
                &mut decoded,
            )
            .expect("export round-trip");
        assert_eq!(decoded, export);
    }

    #[test]
    fn atomic_ops_requests_match_go() {
        // Import → RemoveRequests = [input_id] on the source chain.
        let import = golden_import();
        let (chain, reqs) = import.atomic_ops();
        assert_eq!(chain, id32(0x22));
        assert!(reqs.put.is_empty());
        assert_eq!(reqs.remove.len(), 1);
        assert_eq!(
            hex::encode(&reqs.remove[0]),
            "073baa2c7cbe84111ec1b5a2dba50afa546640f5f66ce3828be5c57ed9d77d93"
        );

        // Export → PutRequests = [Element{key, value, traits}] on the dest chain.
        let export = golden_export();
        // The signed-tx id from the Go golden dump (Sign(Codec, nil) over the
        // unsigned-only Tx).
        let tx_id = Id::from_slice(
            &hex::decode("06ceeed2e0b93c5cb22055711767ce439ce220c94297136f64dd54438cd4fddc")
                .expect("decode tx id"),
        )
        .expect("tx id");
        let (chain, reqs) = export.atomic_ops(tx_id).expect("export atomic ops");
        assert_eq!(chain, id32(0x33));
        assert!(reqs.remove.is_empty());
        assert_eq!(reqs.put.len(), 1);
        let elem = &reqs.put[0];
        assert_eq!(
            hex::encode(&elem.key),
            "c3da83f18816ccfe3294337d6d15188b13fc058de87d4b6778b15c2640993bca"
        );
        assert_eq!(elem.traits.len(), 1);
        assert_eq!(
            hex::encode(&elem.traits[0]),
            "0505050505050505050505050505050505050505"
        );
        assert_eq!(hex::encode(&elem.value), EXPORT_UTXO_VALUE_HEX);
    }

    #[test]
    fn signed_tx_initialize_roundtrips() {
        let mut tx = Tx::new(AtomicTx::Import(golden_import()));
        tx.initialize().expect("initialize");
        assert_ne!(tx.id(), Id::EMPTY);
        assert_eq!(tx.id(), Id::from(hashing::sha256(tx.bytes())));

        let parsed = Tx::parse(tx.bytes()).expect("parse");
        assert_eq!(parsed.unsigned, tx.unsigned);
        assert_eq!(parsed.creds, tx.creds);
        assert_eq!(parsed.id(), tx.id());
    }
}
