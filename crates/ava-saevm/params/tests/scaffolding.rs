// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! M7.1 scaffolding check: the crate compiles and exposes the Tau constant.

#[test]
fn tau_seconds_is_five() {
    assert_eq!(ava_saevm_params::TAU_SECONDS, 5);
}
