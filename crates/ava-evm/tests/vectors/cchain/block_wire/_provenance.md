# C-Chain block wire golden vectors — provenance (M6.7)

Source of truth: **coreth** (grafted into avalanchego).

- avalanchego git rev: `fb174e8925ba86e9ba5fd84eb4d6e5e8c23ffc11`
- coreth module: `github.com/ava-labs/avalanchego/graft/coreth`
- go version: `go1.25.10 darwin/arm64`
- spec: `specs/10-cchain-evm-reth.md` §9.3 (block bytes wire format), §6.2
  (in-block atomic-tx encoding), `specs/02-testing-strategy.md` §6 (golden
  vectors).

## Block wire format (coreth / libevm)

Block bytes = `rlp.EncodeToBytes(block)` =
`RLP([Header, Txs, Uncles, Version(uint32), ExtData(bytes)])`
(coreth `plugin/evm/customtypes/block_ext.go` `BlockRLPFieldsForEncoding` —
the geth `Withdrawals` field is replaced by the two Avalanche fields
`Version` + `ExtData`).

The **Header** uses the libevm header-extra layout
(coreth `plugin/evm/customtypes/gen_header_serializable_rlp.go`):
the 15 standard Ethereum header fields, then `ExtDataHash` (always present,
field 16), then an optional tail included with the standard "any later field
present ⇒ all earlier present" RLP-optional discipline:
`BaseFee` (AP3), `ExtDataGasUsed` (AP4), `BlockGasCost` (AP4),
`BlobGasUsed` (EIP-4844), `ExcessBlobGas` (EIP-4844),
`ParentBeaconRoot` (EIP-4788), `TimeMilliseconds` (Granite),
`MinDelayExcess` (Granite).

Block ID / hash = `keccak256(headerRLP)` (coreth `ethtypes.RLPHash(header)`).

`ExtData` carries the atomic txs: post-ApricotPhase5 it is the AP5 **batch**
encoding `atomic.Codec.Marshal(0, []*Tx{...})` (avalanchego linear codec, NOT
RLP); empty (`nil`) when there are no atomic txs. `ExtDataHash` =
`CalcExtDataHash(extData)` = `EmptyExtDataHash` (= `keccak256(rlp(nil))` =
`56e81f17…b421`) when `extData` is empty, else `keccak256(rlp(extData))`.

## Vectors

### `plain_block`
AP3 (London) block 1 with one value-transfer EVM tx and no atomic txs. The
`block_rlp` / `block_hash` are copied verbatim from the (read-only) M6.6
reexecute fixture `tests/vectors/cchain/reexecute/genesis_to_1/genesis_to_1.json`
(`block1_rlp` / `block1_hash`). Re-confirmed by the M6.7 scratch Go test
(`rlp.DecodeBytes` then `rlp.EncodeToBytes` is byte-identical; `block.Hash()`
matches). Header optional tail = `[BaseFee]` only (AP3, no AP4 fields).

### `atomic_block`
AP4+ block 1 with zero EVM txs and one signed atomic Import tx in `ExtData`
(AP5 batch). Header optional tail = `[BaseFee, ExtDataGasUsed, BlockGasCost]`.
Constructed by the M6.7 scratch Go test in
`plugin/evm/customtypes/` using the `atomic` package: a deterministic Import tx
(see the JSON `_comment` for fields) signed with the fixed key
`0x56289e99c94b6912bfc12adc093c9b51124f0dc54ac7a766b2bc5ccf558d8027`, embedded
via `customtypes.NewBlockWithExtData(header, nil, nil, nil, hasher, extData,
false)`. `ExtDataGasUsed = len(extData) = 311`. Round-trip byte-identical and
hash-stable.

## Reproduction

A scratch test `plugin/evm/customtypes/zz_scratch_blockwire_test.go` was added
to coreth, run with
`go test ./plugin/evm/customtypes/ -run 'TestScratchDecodeBlock1|TestScratchAtomicBlock' -v`,
its `SCRATCH …` stdout captured here, then **deleted**. `../avalanchego` was
left git-clean (verified with `git -C ../avalanchego status`).
