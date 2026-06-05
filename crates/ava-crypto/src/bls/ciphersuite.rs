// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! BLS domain-separation tags (DSTs) — byte-exact with avalanchego.
//!
//! Verbatim from `utils/crypto/bls/ciphersuite.go`. These bytes are
//! consensus-affecting; they MUST match Go exactly. Owning spec:
//! `specs/03-core-primitives.md` §3.5.

/// DST for plain signatures (warp / app messages). Go `ciphersuiteSignature`.
pub const CIPHERSUITE_SIGNATURE: &[u8] = b"BLS_SIG_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";

/// DST for proof-of-possession (PoP + signed-IP). Go `ciphersuiteProofOfPossession`.
pub const CIPHERSUITE_POP: &[u8] = b"BLS_POP_BLS12381G2_XMD:SHA-256_SSWU_RO_POP_";
