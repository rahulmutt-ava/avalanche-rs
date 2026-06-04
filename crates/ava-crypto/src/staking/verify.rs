// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Staking-cert signature verification.
//!
//! TODO(M0.20): `check_signature(cert, msg, sig)` = `sha256(msg)` then
//! RSA-PKCS1v15/SHA-256 or ECDSA `VerifyASN1`.
//! Owning spec: `specs/03-core-primitives.md` §3.6.
