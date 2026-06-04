// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Strict staking-cert parser (matches Go's accept/reject set exactly).
//!
//! TODO(M0.20): strict ASN.1 SPKI walk via `x509-parser` plus explicit checks:
//! `MAX_CERTIFICATE_LEN = 2*1024` (`CertificateTooLarge`); RSA modulus exactly
//! 2048/4096 + exponent 65537 + positive odd modulus; ECDSA P-256 only; unknown
//! alg -> `UnknownPublicKeyAlgorithm`. Must reject the `large_rsa_key` case.
//! Owning spec: `specs/03-core-primitives.md` §3.6, `specs/25` §8.1.
