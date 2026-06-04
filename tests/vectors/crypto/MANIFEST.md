# tests/vectors/crypto

Golden cryptographic vectors. Produced by `tools/extract-vectors` (M0.2).
Owning spec: `specs/03-core-primitives.md` §3.1–§3.6, §8 items 6–7;
`specs/25-key-management-and-signing.md` §8.1.

> **Committed** (avalanchego `fb174e8`; see `../manifest.json`).

| File | Schema | Consumed by |
|---|---|---|
| `addr.json` | `[{ pubkey_hex, address_hex, checksum4_hex }]` (33-byte compressed pubkey → 20-byte `ripemd160(sha256)` address; `checksum4` = last 4 of `sha256(pubkey)`) | `tests/hashing.rs` (M0.13) |
| `encodings.json` | `{ hex: [{ payload_hex, hex, hex_nc, hex_c }], bech32: [{ alias, hrp, payload_hex, bech32, formatted }] }` (`formatted` = `"alias-" + bech32`) | `tests/golden_encodings.rs` (M0.17) |
| `secp.json` | `[{ priv_hex, priv_string, pub_compressed_hex, address_hex, eth_address_hex, hash_hex, sig_hex, high_s_sig_hex }]` — `sig`/`high_s_sig` are `[r‖s‖v]` (65B); `high_s_sig_hex` MUST be rejected by `verify_sig_format` | `tests/golden_secp.rs` (M0.18) |
| `bls.json` | `{ secret_hex(32), pub_compressed_hex(48), pop_hex(96), msg_hex, sig_hex(96), agg_secrets_hex[], agg_sig_hex, agg_pub_compressed_hex, dst_signature, dst_pop }` | `tests/golden_bls.rs` (M0.19) |
| `nodeid.json` | `{ _note, cases: [{ cert_der_hex, node_id }] }` — ECDSA P-256 certs (random per generation; valid der→NodeID mappings). **TODO:** add the `large_rsa_key` REJECT case (RSA-3072) for M0.20 | `tests/golden_nodeid.rs` (M0.20) |
| `signer.key` | raw 32-byte big-endian BLS secret key (matches `bls.json` `secret_hex`) | `tests/local_signer.rs` (M0.21) |
