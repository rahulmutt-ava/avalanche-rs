// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Crate-links smoke test (M4.1): confirms the `ava-platformvm` crate builds and
//! exposes its codec-version constant.

// This integration test exercises only the crate's public constant; the dev-deps
// declared for the richer test suites are unused here.
#![allow(unused_crate_dependencies)]

#[test]
fn crate_links() {
    assert_eq!(ava_platformvm::CODEC_VERSION, 0);
}
