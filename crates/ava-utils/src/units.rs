// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Size + denomination unit constants.
//!
//! Mirrors Go `utils/units/bytes.go` and `utils/units/avax.go`. 1 AVAX = 1e9
//! nAVAX.
//! Owning spec: `specs/03-core-primitives.md` §4.3, `specs/21-fee-economics-math.md`.

// Byte-size units (Go `utils/units/bytes.go`).

/// One kibibyte (1024 bytes).
pub const KIB: u64 = 1024;
/// One mebibyte (1024 KiB).
pub const MIB: u64 = 1024 * KIB;
/// One gibibyte (1024 MiB).
pub const GIB: u64 = 1024 * MIB;

// AVAX denominations (Go `utils/units/avax.go`). 1 AVAX = 1_000_000_000 nAVAX.

/// One nano-AVAX — the base accounting unit.
pub const NANO_AVAX: u64 = 1;
/// One micro-AVAX (1000 nAVAX).
pub const MICRO_AVAX: u64 = 1000 * NANO_AVAX;
/// One schmeckle (49 \* 538 \* 73 nAVAX), matching Go's `Schmeckle`.
pub const SCHMECKLE: u64 = 49 * 538 * 73 * NANO_AVAX;
/// One milli-AVAX (1000 µAVAX).
pub const MILLI_AVAX: u64 = 1000 * MICRO_AVAX;
/// One AVAX (1000 mAVAX = 1e9 nAVAX).
pub const AVAX: u64 = 1000 * MILLI_AVAX;
/// One kilo-AVAX (1000 AVAX).
pub const KILO_AVAX: u64 = 1000 * AVAX;
/// One mega-AVAX (1000 kAVAX).
pub const MEGA_AVAX: u64 = 1000 * KILO_AVAX;
