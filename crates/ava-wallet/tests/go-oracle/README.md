# ava-wallet Go-oracle vector emitters

Scratch `_test.go` emitters that produce the golden wallet tx vectors in
`../vectors/wallet/{p,x,c}.json` from a live avalanchego checkout (the M7.29
in-repo-emitter pattern). They are **never** compiled into avalanche-rs; they
are committed here so the vectors are reproducible.

## How to (re)emit

1. Copy each emitter into the matching avalanchego wallet package
   (avalanchego @ the `go_commit` recorded in the vector provenance blocks,
   currently `cc3b103b91173f5e8b89b1b31aea0816766c8ada`):

   ```sh
   AG=/path/to/avalanchego
   cp wallet_avalanche_rs_vectors_p_test.go $AG/wallet/chain/p/
   cp wallet_avalanche_rs_vectors_x_test.go $AG/wallet/chain/x/
   cp wallet_avalanche_rs_vectors_c_test.go $AG/wallet/chain/c/
   ```

2. Run them env-gated (CGO + Go version per the avalanchego `go.mod`):

   ```sh
   cd $AG
   export AVAX_RS_GO_COMMIT=$(git rev-parse HEAD)
   AVAX_RS_OUT=/tmp/p.json go test -run TestAvalancheRsPVectors ./wallet/chain/p/
   AVAX_RS_OUT=/tmp/x.json go test -run TestAvalancheRsXVectors ./wallet/chain/x/
   AVAX_RS_OUT=/tmp/c.json go test -run TestAvalancheRsCVectors ./wallet/chain/c/
   ```

3. Commit the emitted files as
   `crates/ava-wallet/tests/vectors/wallet/{p,x,c}.json`.

4. **Delete the emitters from the avalanchego tree** — leave it clean:

   ```sh
   rm $AG/wallet/chain/{p,x,c}/wallet_avalanche_rs_vectors_*_test.go
   ```

## Fixtures

All three emitters share the same deterministic fixture style: Go
`secp256k1.TestKeys()`, `ids.Empty.Prefix(...)` asset/chain ids, UTXOs ordered
by `utxotest.NewDeterministicChainUTXOs` (canonical UTXOID order — the order
`ava-wallet`'s backends always use), and a fixed
`WithMinIssuanceTime(1_700_000_000)`.

Not covered (deliberately): X-chain `OperationTx` (mint FT/NFT/property) —
`ava-avm` has no typed fx-operation types yet (M5 §5.5 follow-up), so the Rust
builder/signer defer it (`Error::UnsupportedTxType`).
