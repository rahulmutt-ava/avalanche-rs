// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

use std::process::Command;

#[test]
fn binary_answers_version_and_help() {
    let exe = env!("CARGO_BIN_EXE_avalanchers");

    let v = Command::new(exe).arg("--version").output().unwrap();
    assert!(v.status.success());
    assert!(String::from_utf8_lossy(&v.stdout).contains("avalanchers/"));

    let h = Command::new(exe).arg("--help").output().unwrap();
    assert!(h.status.success());
}
