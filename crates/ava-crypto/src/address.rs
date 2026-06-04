// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Bech32 chain-prefixed addresses (`X-avax1...`, `P-fuji1...`).
//!
//! TODO(M0.17): `format_bech32`/`parse_bech32` (8<->5-bit, pad=true, standard
//! bech32 not bech32m, via `bech32 = "0.11"`); `format`/`parse` chain-prefixed,
//! HRP from `ava_types::constants::get_hrp`; `parse` splits on the first `-`
//! (<=2 parts; `NoSeparator`).
//! Owning spec: `specs/03-core-primitives.md` §3.3, `specs/15` §4.4.
