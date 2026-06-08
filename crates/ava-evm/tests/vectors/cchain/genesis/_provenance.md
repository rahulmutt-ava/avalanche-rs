# C-Chain genesis golden vectors — provenance (M6.8)

Source of truth: **avalanchego** embedded genesis + **coreth** `core.Genesis`.

- avalanchego git rev: `fb174e8925ba86e9ba5fd84eb4d6e5e8c23ffc11`
- coreth module: `github.com/ava-labs/avalanchego/graft/coreth`
- go version: `go1.25.10 darwin/arm64`
- spec: `specs/10-cchain-evm-reth.md` §11.1 (genesis JSON), §8.3
  (timestamp-keyed `precompileUpgrades`), §5/§17.2.1 (5-field libevm
  `StateAccount` RLP — M6.30), §9.3 (block header layout / block ID),
  `specs/02-testing-strategy.md` §6 (golden vectors).

## `mainnet.json` / `fuji.json`

The verbatim `cChainGenesis` JSON string embedded in
`avalanchego/genesis/genesis_{mainnet,fuji}.json`, pretty-printed. Each is a
coreth `core.Genesis`: a `config` (chain id + block-0 Ethereum forks, no
Avalanche-phase timestamps — those come from the node config / `ava_version`),
header scalar fields (`nonce`/`timestamp`/`extraData`/`gasLimit`/`difficulty`/
`mixHash`/`coinbase`/`number`/`gasUsed`/`parentHash`), and a single `alloc`
entry: the contract at `0x0100000000000000000000000000000000000000` (the
native-asset-call deployer contract) with `code` and `balance = 0x0`.

Mainnet and Fuji differ ONLY in `config.chainId` (43114 vs 43113). The genesis
header carries no chain id, and the alloc + every header field are identical, so
both networks have the **same** genesis state root and block ID.

## `expected.json` — Go-authoritative roots

Captured by a scratch Go test `core/zz_scratch_genesis_test.go` added to coreth:
it unmarshals each `cChainGenesis` string into a `core.Genesis` and calls
`g.ToBlock()` (coreth `core/genesis.go:toBlock`), then prints `blk.Root()` (the
state root) and `blk.Hash()` (the block ID = `keccak256(RLP(header))` over the
libevm header layout).

| field | value (both networks) |
|---|---|
| genesis state root | `0xd65eb1b8604a7aa497d41cd6372663785a5f809a17bd192edb86658ef24e29cc` |
| genesis block ID | `0x31ced5b9beb7f8782b014660da0cb18cc409f121f408186886e1ca3e8eeca96b` |

Header fields confirmed by the scratch test: `gasLimit = 100000000` (0x5f5e100),
`timestamp = 0`, `number = 0`, `difficulty = 0`, `nonce = 0`, `extra = 0x00`,
`coinbase = 0x0…0`, `baseFee = nil` (AP3 not active at genesis), `uncleHash =`
the empty-ommers hash, `txRoot = receiptRoot =` the empty-trie root. The genesis
header's `ExtDataHash` is the **zero hash** (coreth's `toBlock` leaves the
`ExtDataHash` field at its zero value — the genesis block has no ExtData and the
hash is never computed), NOT `EmptyExtDataHash` (`56e81f17…b421`).

The genesis state root materializes the alloc via the **5-field** libevm
`StateAccount` RLP (`ava-evm::state::rlp_account`, M6.30); the contract account's
`code_hash = keccak256(code)` is committed and its bytecode seeded into the side
store.

## Reproduction

A scratch test `core/zz_scratch_genesis_test.go` was added to coreth, run with
`go test ./core/ -run TestScratchCChainGenesisRoot -v`, its `SCRATCH …` stdout
captured here, then **deleted**. `../avalanchego` was left git-clean (verified
with `git -C ../avalanchego status`).
