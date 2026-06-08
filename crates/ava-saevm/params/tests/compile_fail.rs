// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! trybuild gate: a `BlockInstant` must NOT support raw-integer time math
//! (`- 5u64`). This is the structural `tausecondslint` — the only way to move a
//! `BlockInstant` is via `minus`/`plus` taking a `Duration`.

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/compile_fail/*.rs");
}
