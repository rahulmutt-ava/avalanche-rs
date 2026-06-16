// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fixture (NOT compiled): clean cases that MUST NOT be flagged.
//!  - monotonic `Instant::now()` (latency timing, not a hazard-#5 violation),
//!  - a wall-clock read suppressed by an inline `// determinism-allow:` reason,
//!  - a `HashMap` field on a struct that does NOT derive the codec,
//!  - a `BTreeMap` field on an `#[derive(AvaCodec)]` struct (deterministic order).

use std::collections::{BTreeMap, HashMap};
use std::time::{Instant, SystemTime};

pub fn latency_start() -> Instant {
    Instant::now()
}

pub fn allowed_wall() -> SystemTime {
    SystemTime::now() // determinism-allow: fixture for the inline-annotation path
}

pub struct NotCodec {
    pub cache: HashMap<u32, u64>,
}

#[derive(AvaCodec)]
pub struct OkCodec {
    pub entries: BTreeMap<u32, u64>,
}
