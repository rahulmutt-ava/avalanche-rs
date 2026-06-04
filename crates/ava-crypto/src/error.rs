// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `ava-crypto` error enum (`thiserror`).
//!
//! TODO: seed and extend per task — `specs/03-core-primitives.md` §7 and
//! `specs/25-key-management-and-signing.md` §7.1:
//! M0.13 (base), M0.17 (`MissingHexPrefix, BadChecksum, Base58Decoding,
//! NoSeparator`), M0.18 (`MutatedSig, Compressed`), M0.19/M0.21
//! (`FailedSecretKeyDeserialize`), M0.20 (the cert family:
//! `CertificateTooLarge, UnsupportedRSAModulusBitLen,
//! UnsupportedRSAPublicExponent, UnknownPublicKeyAlgorithm`).
