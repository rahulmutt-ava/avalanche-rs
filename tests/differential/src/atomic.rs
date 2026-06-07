// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Cross-chain atomic (X↔P import/export) differential seam (ATOMIC-1, specs 09
//! §9, 00 §11.1.7).
//!
//! SCAFFOLD: the live two-binary `differential::atomic_xp` arm is gated behind
//! the (unimplemented) [`LockstepDriver`](crate::LockstepDriver) — its
//! `replay_recorded` is owned by tier-X task X.13 and there is no live mode yet.
//! Until then the REAL per-PR ATOMIC-1 gate lives in
//! `crates/ava-avm/tests/atomic_xp.rs` (recorded / self-consistent + Go-vector
//! mode against the real `ava-chains` shared-memory backend), matching the
//! M5.5/M5.15 self-consistent-golden precedent.
//!
//! This module contributes the reusable, driver-independent piece: a normalized
//! [`Observation`] collector over an exported UTXO's shared-memory value bytes,
//! so when the live two-binary mode lands (X.13/X.15) the same `Observation`
//! shape can be compared between the Go and Rust nodes after an export.
//
// TODO(X.13/X.15 live mode): wire this collector into `LockstepDriver` so an
// `Action::IssueTx`(ExportTx) followed by `AwaitFinalization` captures the
// peer-chain shared-memory `get(...)` bytes on BOTH the Go and Rust nodes and
// asserts observation equality (the live `differential::atomic_xp`).

use crate::observation::Observation;

/// Build a normalized [`Observation`] of a single exported cross-chain UTXO:
/// the shared-memory element `key` (the `InputID`) and `value` (the marshalled
/// `avax.UTXO` bytes), hex-encoded so two implementations compare equal.
///
/// The pair `(key, value)` is exactly what a peer chain reads back via
/// `SharedMemory::get`; comparing it across implementations is the ATOMIC-1
/// observation point.
#[must_use]
pub fn exported_utxo_observation(key: &[u8], value: &[u8]) -> Observation {
    Observation {
        fields: vec![
            ("atomic.export.key".to_owned(), hex_lower(key)),
            ("atomic.export.value".to_owned(), hex_lower(value)),
        ],
    }
    .normalized()
}

/// Lower-hex encode without pulling a dependency into the harness scaffold.
fn hex_lower(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        s.push(char::from_digit(u32::from(b >> 4), 16).unwrap_or('0'));
        s.push(char::from_digit(u32::from(b & 0x0f), 16).unwrap_or('0'));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_is_normalized_and_hex_encoded() {
        let obs = exported_utxo_observation(&[0xde, 0xad], &[0x00, 0xff]);
        // Fields are sorted by `normalized()` and hex-encoded lower-case.
        assert_eq!(
            obs.fields,
            vec![
                ("atomic.export.key".to_owned(), "dead".to_owned()),
                ("atomic.export.value".to_owned(), "00ff".to_owned()),
            ]
        );
    }
}
