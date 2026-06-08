// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fuzz target: decode-never-panics over arbitrary bytes for the C-Chain atomic
//! Import/Export transaction codec (spec 10 §6.1/§6.2, M6.14/M6.28).
//!
//! Drives `atomic::tx::Tx::parse` with arbitrary input, asserting only that
//! parsing never panics — errors are expected and ignored. The atomic codec is
//! the Avalanche linear codec (NOT RLP); the parser recurses into the
//! `UnsignedImportTx`/`UnsignedExportTx` body and the `FxCredential` list, so
//! the full atomic-tx decode tree is exercised.
//!
//! Where parsing succeeds, a round-trip is asserted: the cached signed bytes
//! reproduced by `Tx::parse` must equal the fuzz input (the coreth invariant:
//! `parse → bytes` returns the original wire bytes verbatim, mirroring
//! `Tx.Initialize` → `signed bytes = codec.Marshal(tx)`).

#![no_main]

use libfuzzer_sys::fuzz_target;

use ava_evm::atomic::tx::Tx;

fuzz_target!(|data: &[u8]| {
    // Decode-never-panics: arbitrary bytes must not cause a panic, only errors.
    // `Tx::parse` runs the full atomic linear codec decode tree (unsigned tx
    // discriminant → ImportTx or ExportTx body → transferred inputs/outputs →
    // credentials).
    if let Ok(tx) = Tx::parse(data) {
        // Round-trip stability (coreth `Tx.Initialize` invariant): `Tx::parse`
        // caches `signed_bytes = data` in `tx.bytes`, so the cached bytes must
        // be byte-identical to the fuzz input.
        assert_eq!(
            tx.bytes(),
            data,
            "parse → bytes round-trip must be byte-identical"
        );
    }
});
