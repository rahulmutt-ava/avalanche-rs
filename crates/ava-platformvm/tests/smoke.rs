// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Crate-links smoke test (M4.1): confirms the `ava-platformvm` crate builds and
//! exposes its codec-version constant.

#[test]
fn crate_links() {
    assert_eq!(ava_platformvm::CODEC_VERSION, 0);
}
