# C-Chain atomic Import/Export tx golden vectors — provenance

**Provenance: Go-EXECUTED** against the coreth atomic package on
`go1.25.10 darwin/arm64`. These are not hand-derived.

## How they were generated

A scratch Go test `zz_golden_dump_test.go` was placed in the coreth atomic
package and run, then deleted:

```
cd /Users/rahul.muttineni/avalanchego/graft/coreth
go test ./plugin/evm/atomic/ -run TestGoldenDump -v
```

The test lives in `package atomic` so it can use the unexported `Codec`
(from `plugin/evm/atomic/codec.go`) and the `EVMOutput` / `EVMInput` /
`UnsignedImportTx` / `UnsignedExportTx` / `Tx` types directly.

Module: `github.com/ava-labs/avalanchego/graft/coreth`
Source files exercised:
- `plugin/evm/atomic/tx.go`        — `EVMOutput`, `EVMInput`, `Tx`, `X2CRateUint64`, `TxBytesGas`, `EVMOutputGas`, `EVMInputGas`
- `plugin/evm/atomic/import_tx.go` — `UnsignedImportTx`, `(*UnsignedImportTx).AtomicOps`
- `plugin/evm/atomic/export_tx.go` — `UnsignedExportTx`, `(*UnsignedExportTx).AtomicOps`
- `plugin/evm/atomic/codec.go`     — `Codec`, `CodecVersion = 0` (the atomic linear codec)
- `vms/secp256k1fx/input.go`       — `CostPerSignature = 1000`

## Inputs (deterministic)

| Field | Value |
|-------|-------|
| network_id | 1 |
| blockchain_id | 0x11 × 32 |
| source_chain | 0x22 × 32 |
| destination_chain | 0x33 × 32 |
| avax asset id | 0xAA × 32 |
| import imported input | UTXOID{tx_id=0x44×32, output_index=1}, asset=0xAA, secp256k1fx.TransferInput{amt=5000, sig_indices=[0]} |
| import out | EVMOutput{addr=0x01×20, amount=4999, asset=0xAA} |
| export in | EVMInput{addr=0x02×20, amount=3000, asset=0xAA, nonce=7} |
| export out | TransferableOutput{asset=0xAA, secp256k1fx.TransferOutput{amt=3000, locktime=0, threshold=1, addrs=[0x05×20]}} |

## Key facts captured

- Two encodings are captured per unsigned tx:
  - `struct_codec_hex` = `Codec.Marshal(0, concreteStructPtr)` — the BARE struct:
    `version(2) ‖ fields`, NO interface type_id (Go's reflectcodec emits no type
    prefix when the static type is a concrete pointer, not an interface).
  - `interface_codec_hex` = `Codec.Marshal(0, &iface)` where
    `var iface UnsignedAtomicTx = utx` — the interface form the signed `Tx`
    envelope and shared-memory framing carry: it is the struct bytes with a
    4-byte `u32` type_id (`UnsignedImportTx`=0, `UnsignedExportTx`=1) inserted
    right after the 2-byte version prefix. Both were dumped from Go and verified;
    the test reconstructs the interface form from the struct form via
    `splice_type_id` and checks it against the Rust `AtomicTx` enum encoding.
- The atomic codec's type-id registration (codec.go init) differs from the
  X-Chain codec: 0=UnsignedImportTx, 1=UnsignedExportTx, [skip 3], 5=TransferInput,
  [skip 1], 7=TransferOutput, [skip 1], 9=Credential, 10=Input, 11=OutputOwners.
  For the secp fx payloads that atomic txs use (TransferInput=5, TransferOutput=7,
  Credential=9) the type-ids coincide with the X-Chain codec, so reusing the
  X-Chain component encodings is byte-exact.
- Import `AtomicOps` → `(source_chain, Requests{RemoveRequests=[in.InputID()]})`.
  `InputID() = sha256(be64(output_index) ++ tx_id)` (`ids.ID.Prefix`).
- Export `AtomicOps` → `(destination_chain, Requests{PutRequests=[Element{
  key=utxo.InputID(), value=Codec.Marshal(0, utxo), traits=out.Addresses()}]})`.
  The exported UTXO uses `TxID = signed-tx ID`, `OutputIndex = i` (0-based over
  the exported outputs only). The signed-tx ID here is over an unsigned-only Tx
  (no credentials), `Sign(Codec, nil)`.

## `gas_used` block (M6.29 fold-in)

Go-EXECUTED `GasUsed` values for the corpus txs, added by the M6.29 exit gate
after the M8.26 wallet differential exposed a +77-gas overcount in
`atomic/mempool.rs::gas_used` (it priced the SIGNED envelope).

- **Emitter:** `tests/differential/go-oracle/atomic_tx_gas_emitter_test.go`,
  dropped into `graft/coreth/plugin/evm/atomic/` and run as
  `AVAX_RS_EMIT_ATOMIC_GAS=1 go test -run TestEmitAtomicTxGasUsed`.
- **Go tree:** avalanchego `5896c92fee23c2eff53d557dceeb89f1a6218224`
  (graft/coreth), `go1.25.10 darwin/arm64`, 2026-06-12. Tree left git-clean.
- **What it pins:** coreth `Metadata.Bytes()` (`metadata.go:30`) returns the
  **unsigned** bytes (the Go name is misleading; `SignedBytes()` is the full
  envelope), and `GasUsed(fixedFee)` =
  `len(unsignedBytes)*TxBytesGas + Σ len(SigIndices)*CostPerSignature
  [+ ap5.AtomicTxIntrinsicGas iff fixedFee]`
  (`import_tx.go:136`, `export_tx.go:134`, `tx.go:340`).
- Each corpus tx signed with ONE secp credential of ONE 65-byte signature
  (signature content does not affect gas): unsigned = 230 B, signed = 307 B
  (+77 = 4 creds-len + 4 cred type_id + 4 sigs-len + 65 sig), so
  `GasUsed(true)` = 230 + 1000 + 10000 = **11230** for both import and export.
- Asserted by `crates/ava-evm/tests/atomic_mempool.rs::gas_used_matches_coreth_oracle`.
