# PORTING.md — `ava-crypto`

Parity against avalanchego `utils/hashing/`, `utils/formatting/`,
`utils/crypto/secp256k1/`, `utils/crypto/bls/`, `staking/`. One row per upstream
Go test; status `todo` / `wip` / `ported` / `na`. No `wip` rows at the M0.25
exit gate.

Owning tasks: M0.13 (hashing), M0.17 (formatting + bech32), M0.18 (secp256k1),
M0.19 (BLS), M0.20 (staking certs), M0.21 (Signer / LocalSigner).

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `utils/hashing/hashing_test.go` | `tests/hashing.rs` | todo |
| `utils/formatting/encoding_test.go` | `tests/golden_encodings.rs` | todo |
| `utils/formatting/address_test.go` | `tests/golden_encodings.rs` | todo |
| `utils/crypto/secp256k1/rfc6979_test.go` | `tests/golden_secp.rs` | todo |
| `utils/crypto/secp256k1/secp256k1_test.go` | `tests/golden_secp.rs` | todo |
| `utils/crypto/bls/signer_test.go` | `tests/golden_bls.rs` | todo |
| `utils/crypto/bls/*_test.go` (agg/PoP) | `tests/golden_bls.rs` | todo |
| `staking/tls_test.go` | `tests/golden_nodeid.rs` | todo |
| `staking/parse_test.go` | `tests/golden_nodeid.rs` | todo |
| `utils/crypto/bls/signers/local/*_test.go` | `tests/local_signer.rs` | todo |
