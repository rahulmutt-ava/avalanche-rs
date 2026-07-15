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

## `live_local_block1.json` (M9.15 rung 5 — live mixed_network capture)

- Captured **2026-07-15** from the live `mixed_network` differential harness
  (`tests/differential/tests/mixed_network.rs`, `--features live`): a
  5-validator **Go** local network (oracle binary
  `~/avalanchego/build/avalanchego` @ `96897293a2249cbce94411466618924ec24199c8`,
  rpcchainvm=45) built and accepted C-Chain block 1 from one legacy transfer
  tx; the Rust follower fetched it via `Get`→`Put` and the engine's rung-5
  capture instrumentation (`SnowmanEngine::put` `container_hex` debug line)
  hex-dumped the exact wire bytes from `<workdir>/rust/logs/main.log`.
- The 791-byte container is a proposervm **unsigned post-fork** block
  (codec v0: parentID `608ddb…07b8b1` = the local C genesis ethhash, timestamp,
  pChainHeight 0, empty cert/signature) wrapping the 725-byte inner coreth
  block (`RLP([Header, Txs, Uncles, Version, ExtData])`). Container id
  (`sha256`) = `2vWVdmMroWuCZxU3YJ1gQzRVfMwTWT38SajCTEcuSfn5eNuisE` — the id
  every Go validator's chits named.
- Cancun is active at the local genesis timestamp (Etna alignment), so the
  inner header's optional tail carries `parentBeaconRoot = 0x0` (and Granite's
  `timestampMs`/`minDelayExcess`); coreth executes the block through
  `ProcessBeaconBlockRoot` (`core/state_processor.go:97`).
- Regression pinned by `tests/live_block_adopt.rs` (and the engine-level
  `avalanchers` test `c_chain_follower_adopts_live_go_block`): the follower
  must parse, verify (execute to the header state root
  `0feb3745…850576f3`), and accept this container over the local genesis.
