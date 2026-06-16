// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Fixture (NOT compiled): a `HashMap` field on an `#[derive(AvaCodec)]` struct.
//! Expected: hazard #1 finding on the map field (nondeterministic serialize order).

use std::collections::HashMap;

#[derive(AvaCodec)]
pub struct Bad {
    pub entries: HashMap<u32, u64>,
}
