// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Payload encodings — Hex / HexC / HexNC / Json (`utils/formatting`).
//!
//! TODO(M0.17): `Encoding { Hex, HexNC, HexC, Json }` + `encode`/`decode` per
//! `specs/03-core-primitives.md` §3.2 — default `Hex` = `"0x" + hex(payload ++
//! checksum4)`, `HexNC` = `"0x" + hex(payload)`, decode verifies checksum /
//! requires `0x`, `Json` unsupported in this path.
