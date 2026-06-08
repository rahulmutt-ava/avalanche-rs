# Warp precompile golden vectors (C-Chain / EVM, spec 10 §8, spec 20 §7, G4, M6.22)

`selectors.json` pins the byte-exact, consensus-critical constants of the Warp
stateful precompile that `ava-evm` must reproduce to be a drop-in replacement for
coreth's `precompile/contracts/warp`:

- **`address`** — the warp precompile contract address `0x02…05`
  (`module.go::ContractAddress`).
- **`selectors`** — the 4-byte ABI function selectors
  (`keccak256(signature)[:4]`) for the four warp ABI functions
  (`IWarpMessenger.abi`).
- **`event`** — the `SendWarpMessage(address,bytes32,bytes)` event topic0
  (`keccak256(eventSignature)`), emitted by `sendWarpMessage`.
- **`gas.preGranite` / `gas.granite`** — the two `GasConfig` tables
  (`contract.go::preGraniteGasConfig` / `graniteGasConfig`), selected by the
  active fork (spec 20 §7.3). `sendWarpMessageBase = LogGas(375) +
  3*LogTopicGas(375) + addWarpMessageBaseGasCost(20000) +
  WriteGasCostPerSlot(20000) = 41500`, unchanged across forks;
  `perWarpMessageByte = LogDataGas = 8`.

## Provenance (how the constants were derived)

The selectors + event topic are `keccak256` of the canonical Solidity signatures
(verified against the in-repo `graft/coreth` and `graft/subnet-evm`
`IWarpMessenger.abi` via a Go `golang.org/x/crypto/sha3` one-shot). The gas tables
are copied verbatim from coreth `precompile/contracts/warp/contract.go`. They are
asserted both by the named integration test (`tests/warp_precompile.rs`) — which
drives the real `WarpPrecompile` against them — and by the golden-vector
comparison in that test, so a drift in either the constants or the precompile
breaks the test.
