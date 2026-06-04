// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Staking cert + key generation (`rcgen`).
//!
//! TODO(M0.20): `new_cert_and_key_bytes` with the exact Go template — ECDSA
//! P-256, `SerialNumber=0`, `NotBefore` = the Go `January,0` instant
//! (Dec 31 1999), `NotAfter = now + 100y`, `KeyUsage=DigitalSignature`, no SAN;
//! PEM `CERTIFICATE` + PKCS#8 `PRIVATE KEY`; files `0o400`, dir `0o700`.
//! Requires the `rcgen` dependency (see Cargo.toml).
//! Owning spec: `specs/25-key-management-and-signing.md` §2.1.
