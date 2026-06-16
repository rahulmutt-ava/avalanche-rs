// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fixture (NOT compiled): a wall-clock read with no allowlist annotation.
//! Expected: hazard #5 finding on the `SystemTime::now()` line.

use std::time::SystemTime;

pub fn build_time() -> SystemTime {
    SystemTime::now()
}
