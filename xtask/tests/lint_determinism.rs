// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Integration tests for the `lint-determinism` AST pass (specs/24 PART A, X.19).
//!
//! Runs the pass over the planted-violation fixtures under
//! `tests/fixtures/determinism/` and asserts that the genuine violations are
//! flagged while the clean cases (monotonic timer, inline-allowlisted wall clock,
//! non-codec map, `BTreeMap`-in-codec) are not.

use std::path::PathBuf;

// Pulled in transitively via the lib; the test binary does not use these crates
// directly, so acknowledge them for `unused_crate_dependencies` under -D warnings.
use anyhow as _;
use clap as _;
use proc_macro2 as _;
use serde as _;
use serde_json as _;
use sha2 as _;
use syn as _;
use toml as _;
use walkdir as _;
use xtask::lint_determinism::{Hazard, scan_files};

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/determinism")
}

fn fixture(name: &str) -> PathBuf {
    fixtures_dir().join(name)
}

#[test]
fn flags_bare_wallclock_read() {
    let findings =
        scan_files(&[fixture("bad_wallclock.rs")]).expect("scan_files(bad_wallclock.rs)");
    let hits: Vec<_> = findings
        .iter()
        .filter(|f| f.hazard == Hazard::WallClock)
        .collect();
    assert_eq!(hits.len(), 1, "exactly one hazard-#5 finding: {findings:?}");
}

#[test]
fn flags_hashmap_in_codec_type() {
    let findings = scan_files(&[fixture("bad_codecmap.rs")]).expect("scan_files(bad_codecmap.rs)");
    let hits: Vec<_> = findings
        .iter()
        .filter(|f| f.hazard == Hazard::CodecMap)
        .collect();
    assert_eq!(hits.len(), 1, "exactly one hazard-#1 finding: {findings:?}");
}

#[test]
fn does_not_flag_clean_cases() {
    let findings = scan_files(&[fixture("good_clean.rs")]).expect("scan_files(good_clean.rs)");
    assert!(
        findings.is_empty(),
        "monotonic timer + allowlisted wall clock + non-codec map + BTreeMap-in-codec must be clean, got: {findings:?}"
    );
}

#[test]
fn whole_fixture_dir_flags_exactly_the_two_bad_files() {
    let findings = scan_files(&[
        fixture("bad_wallclock.rs"),
        fixture("bad_codecmap.rs"),
        fixture("good_clean.rs"),
    ])
    .expect("scan_files(fixtures)");
    assert_eq!(
        findings.len(),
        2,
        "exactly the two planted bads: {findings:?}"
    );
}
