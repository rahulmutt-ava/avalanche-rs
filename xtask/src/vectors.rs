// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden-vector corpus management (specs/22 §2.2, §6; tier X / X.10–X.12).
//!
//! Implements the Rust side of the corpus gate:
//!
//! * `verify` — JSON schema check (every `*.json` parses), orphan/coverage
//!   check (surface dirs ↔ `manifest.json.surfaces`), and a sha256 checksum
//!   check against the committed `checksums.txt`.
//! * `regen` — (re)write `checksums.txt` from the current corpus (the
//!   deliberate-protocol-change flow).
//! * `diff --against <dir>` — compare the committed corpus against a
//!   freshly-extracted directory (path set difference + per-file byte compare).
//!   Wired for `scripts/vectors_drift.sh`; the Go re-extraction is gated.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use clap::Subcommand;
use sha2::{Digest, Sha256};

/// File name of the committed checksum manifest under `tests/vectors/`.
const CHECKSUMS_FILE: &str = "checksums.txt";

/// File name of the top-level provenance manifest under `tests/vectors/`.
const MANIFEST_FILE: &str = "manifest.json";

/// `xtask vectors <action>` — manage `tests/vectors/`.
#[derive(Subcommand)]
pub enum Action {
    /// Validate the corpus: schema, recompute sha256 vs manifest, orphan check.
    Verify,
    /// Diff the committed corpus against a freshly-extracted directory.
    Diff {
        /// Directory of freshly-extracted vectors (from `tools/extract-vectors`).
        #[arg(long)]
        against: String,
    },
    /// Regenerate vectors (deliberate protocol change flow).
    Regen,
}

/// Dispatch a `vectors` action.
pub fn run(action: Action) -> anyhow::Result<()> {
    let vectors_dir = vectors_dir()?;
    match action {
        Action::Verify => verify(&vectors_dir),
        Action::Regen => regen(&vectors_dir),
        Action::Diff { against } => diff(&vectors_dir, Path::new(&against)),
    }
}

/// Resolve `tests/vectors/` relative to the repo root.
///
/// `xtask` is a workspace member, so `CARGO_MANIFEST_DIR` points at `xtask/`;
/// the repo root is its parent. This is CWD-independent.
fn vectors_dir() -> anyhow::Result<PathBuf> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask manifest dir has no parent (cannot locate repo root)")?;
    Ok(repo_root.join("tests").join("vectors"))
}

/// Returns true if a path is excluded from the checksum corpus: the checksum
/// manifest itself, Bazel build files, and human-readable `*.md` docs.
fn is_excluded(rel: &Path) -> bool {
    if rel == Path::new(CHECKSUMS_FILE) {
        return true;
    }
    match rel.file_name().and_then(|n| n.to_str()) {
        Some("BUILD.bazel") => true,
        _ => rel
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md")),
    }
}

/// Recursively collect every non-excluded vector file under `root`, as paths
/// relative to `root`, sorted.
fn collect_vector_files(root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort();
    Ok(out)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("reading directory {}", dir.display()))?;
    for entry in entries {
        let entry = entry.with_context(|| format!("reading entry in {}", dir.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .with_context(|| format!("stat {}", path.display()))?;
        if file_type.is_dir() {
            walk(root, &path, out)?;
        } else {
            let rel = path
                .strip_prefix(root)
                .with_context(|| format!("{} is not under {}", path.display(), root.display()))?
                .to_path_buf();
            if !is_excluded(&rel) {
                out.push(rel);
            }
        }
    }
    Ok(())
}

/// Format a checksum line: `"<hex sha256>  <relative/path>"` with forward
/// slashes (stable across platforms).
fn checksum_line(hash: &str, rel: &Path) -> String {
    format!("{hash}  {}", rel_to_slash(rel))
}

/// Render a relative path with `/` separators regardless of host OS.
fn rel_to_slash(rel: &Path) -> String {
    rel.components()
        .filter_map(|c| c.as_os_str().to_str())
        .collect::<Vec<_>>()
        .join("/")
}

/// sha256 of a file's bytes as a lowercase hex string.
fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hex(&hasher.finalize()))
}

/// Lowercase hex encoding.
fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len().saturating_mul(2));
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// Compute the sorted `<path> -> <sha256>` map for the current corpus.
fn compute_checksums(root: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for rel in collect_vector_files(root)? {
        let hash = sha256_file(&root.join(&rel))?;
        map.insert(rel_to_slash(&rel), hash);
    }
    Ok(map)
}

/// Parse `checksums.txt` into a sorted `<path> -> <sha256>` map.
fn parse_checksums(text: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    for (i, line) in (1u64..).zip(text.lines()) {
        if line.trim().is_empty() {
            continue;
        }
        let (hash, path) = line
            .split_once("  ")
            .with_context(|| format!("malformed checksum line {i} (expected '<hash>  <path>')"))?;
        map.insert(path.to_string(), hash.to_string());
    }
    Ok(map)
}

/// Render a checksum map to the canonical sorted `checksums.txt` body.
fn render_checksums(map: &BTreeMap<String, String>) -> String {
    let mut body = String::new();
    for (path, hash) in map {
        body.push_str(&checksum_line(hash, Path::new(path)));
        body.push('\n');
    }
    body
}

/// `regen`: (re)write `checksums.txt` from the current corpus.
fn regen(root: &Path) -> anyhow::Result<()> {
    let map = compute_checksums(root)?;
    let body = render_checksums(&map);
    let dest = root.join(CHECKSUMS_FILE);
    std::fs::write(&dest, body).with_context(|| format!("writing {}", dest.display()))?;
    println!(
        "xtask vectors regen: hashed {} vector file(s) -> {}",
        map.len(),
        dest.display()
    );
    Ok(())
}

/// `verify`: JSON schema, orphan/coverage, and checksum checks.
fn verify(root: &Path) -> anyhow::Result<()> {
    if !root.exists() {
        bail!("vectors directory not found: {}", root.display());
    }

    let mut errors: Vec<String> = Vec::new();
    let files = collect_vector_files(root)?;

    // 1. Schema/JSON check: every *.json must parse.
    for rel in &files {
        let is_json = rel
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("json"));
        if !is_json {
            continue;
        }
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if let Err(e) = serde_json::from_str::<serde_json::Value>(&text) {
            errors.push(format!("invalid JSON: {} ({e})", rel_to_slash(rel)));
        }
    }

    // 2. Orphan/coverage check: surface dirs <-> manifest.json.surfaces.
    if let Err(coverage_errs) = coverage_check(root) {
        errors.extend(coverage_errs);
    }

    // 3. Checksum check against committed checksums.txt.
    let checksums_path = root.join(CHECKSUMS_FILE);
    if !checksums_path.exists() {
        bail!(
            "{} is missing — run `cargo xtask vectors regen` to create it",
            checksums_path.display()
        );
    }
    let committed_text = std::fs::read_to_string(&checksums_path)
        .with_context(|| format!("reading {}", checksums_path.display()))?;
    let committed = parse_checksums(&committed_text)?;
    let computed = compute_checksums(root)?;

    let committed_paths: BTreeSet<&String> = committed.keys().collect();
    let computed_paths: BTreeSet<&String> = computed.keys().collect();

    for extra in computed_paths.difference(&committed_paths) {
        errors.push(format!(
            "checksum: file present on disk but missing from {CHECKSUMS_FILE}: {extra}"
        ));
    }
    for missing in committed_paths.difference(&computed_paths) {
        errors.push(format!(
            "checksum: {CHECKSUMS_FILE} lists {missing} but the file is absent on disk"
        ));
    }
    for (path, want) in &committed {
        if let Some(got) = computed.get(path)
            && want != got
        {
            errors.push(format!(
                "checksum mismatch: {path}\n    expected {want}\n    actual   {got}"
            ));
        }
    }

    if errors.is_empty() {
        println!(
            "xtask vectors verify: OK — {} file(s) parsed/hashed, surfaces consistent.",
            computed.len()
        );
        Ok(())
    } else {
        for e in &errors {
            eprintln!("  - {e}");
        }
        bail!(
            "vectors verify FAILED with {} problem(s) (see above). If this is an \
             intentional corpus change, run `cargo xtask vectors regen` and commit \
             the updated {CHECKSUMS_FILE}.",
            errors.len()
        );
    }
}

/// Orphan/coverage check between surface subdirectories and `manifest.json`.
///
/// Returns `Ok(())` when consistent, otherwise a list of human-readable
/// problems. A "surface" is a top-level subdirectory of `tests/vectors/` that
/// contains at least one vector file (recursively).
fn coverage_check(root: &Path) -> Result<(), Vec<String>> {
    let manifest_path = root.join(MANIFEST_FILE);
    let text = match std::fs::read_to_string(&manifest_path) {
        Ok(t) => t,
        Err(e) => return Err(vec![format!("reading {}: {e}", manifest_path.display())]),
    };
    let manifest: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => return Err(vec![format!("{MANIFEST_FILE} is not valid JSON: {e}")]),
    };
    let surfaces = match manifest.get("surfaces").and_then(|s| s.as_object()) {
        Some(s) => s,
        None => return Err(vec![format!("{MANIFEST_FILE} has no `surfaces` object")]),
    };
    let manifest_keys: BTreeSet<&str> = surfaces.keys().map(String::as_str).collect();

    // Discover surface dirs on disk: top-level subdirs containing vector files.
    let mut disk_surfaces: BTreeSet<String> = BTreeSet::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) => return Err(vec![format!("reading {}: {e}", root.display())]),
    };
    let mut errs = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                errs.push(format!("reading entry in {}: {e}", root.display()));
                continue;
            }
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        let files = match collect_vector_files(&path) {
            Ok(f) => f,
            Err(e) => {
                errs.push(format!("walking surface dir {name}: {e}"));
                continue;
            }
        };
        if !files.is_empty() {
            disk_surfaces.insert(name);
        }
    }

    let disk_keys: BTreeSet<&str> = disk_surfaces.iter().map(String::as_str).collect();
    for orphan in disk_keys.difference(&manifest_keys) {
        errs.push(format!(
            "orphan surface: directory `{orphan}/` has vectors but is not a key in \
             {MANIFEST_FILE}.surfaces"
        ));
    }
    for missing in manifest_keys.difference(&disk_keys) {
        errs.push(format!(
            "missing surface: {MANIFEST_FILE}.surfaces lists `{missing}` but no such \
             directory with vectors exists on disk"
        ));
    }

    if errs.is_empty() { Ok(()) } else { Err(errs) }
}

/// `diff --against <dir>`: compare the committed corpus against a freshly
/// extracted directory. Set difference of relative paths + per-file byte
/// compare. Non-zero exit on any drift.
fn diff(root: &Path, against: &Path) -> anyhow::Result<()> {
    if !against.exists() {
        bail!("--against directory not found: {}", against.display());
    }
    let committed = collect_vector_files(root)?;
    let fresh = collect_vector_files(against)?;

    let committed_set: BTreeSet<String> = committed.iter().map(|p| rel_to_slash(p)).collect();
    let fresh_set: BTreeSet<String> = fresh.iter().map(|p| rel_to_slash(p)).collect();

    let mut drift: Vec<String> = Vec::new();
    for only_committed in committed_set.difference(&fresh_set) {
        drift.push(format!("only in committed corpus: {only_committed}"));
    }
    for only_fresh in fresh_set.difference(&committed_set) {
        drift.push(format!("only in freshly-extracted dir: {only_fresh}"));
    }
    for path in committed_set.intersection(&fresh_set) {
        let rel = Path::new(path);
        let a =
            std::fs::read(root.join(rel)).with_context(|| format!("reading committed {path}"))?;
        let b = std::fs::read(against.join(rel))
            .with_context(|| format!("reading extracted {path}"))?;
        if a != b {
            drift.push(format!("byte drift: {path}"));
        }
    }

    if drift.is_empty() {
        println!(
            "xtask vectors diff: no drift — {} file(s) identical.",
            committed_set.len()
        );
        Ok(())
    } else {
        for d in &drift {
            eprintln!("  - {d}");
        }
        bail!(
            "vectors diff FAILED: {} difference(s) between committed corpus and {}",
            drift.len(),
            against.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksum_line_uses_two_spaces_and_slashes() {
        let line = checksum_line(
            "abc123",
            Path::new("saevm").join("blocks").join("b.json").as_path(),
        );
        assert_eq!(line, "abc123  saevm/blocks/b.json");
    }

    #[test]
    fn rel_to_slash_normalizes_separators() {
        let rel: PathBuf = ["a", "b", "c.json"].iter().collect();
        assert_eq!(rel_to_slash(&rel), "a/b/c.json");
    }

    #[test]
    fn hex_lowercase_padded() {
        assert_eq!(hex(&[0x00, 0x0f, 0xff, 0xa0]), "000fffa0");
    }

    #[test]
    fn is_excluded_skips_md_bazel_and_self() {
        assert!(is_excluded(Path::new(CHECKSUMS_FILE)));
        assert!(is_excluded(Path::new("BUILD.bazel")));
        assert!(is_excluded(Path::new("crypto/MANIFEST.md")));
        assert!(!is_excluded(Path::new("crypto/secp.json")));
        assert!(!is_excluded(Path::new("crypto/signer.key")));
        assert!(!is_excluded(Path::new("saevm/atomic/import_tx.bin")));
    }

    #[test]
    fn parse_render_roundtrip() {
        let mut map = BTreeMap::new();
        map.insert("a/b.json".to_string(), "deadbeef".to_string());
        map.insert("z.json".to_string(), "cafef00d".to_string());
        let body = render_checksums(&map);
        // Sorted, two-space separated.
        assert_eq!(body, "deadbeef  a/b.json\ncafef00d  z.json\n");
        let parsed = parse_checksums(&body).expect("parse");
        assert_eq!(parsed, map);
    }

    #[test]
    fn parse_rejects_malformed_line() {
        assert!(parse_checksums("nohashorpath").is_err());
    }

    #[test]
    fn sha256_file_matches_known_vector() {
        // sha256("") = e3b0c442...
        let dir = std::env::temp_dir().join(format!("xtask-vectors-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let f = dir.join("empty");
        std::fs::write(&f, b"").expect("write");
        let got = sha256_file(&f).expect("hash");
        assert_eq!(
            got,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
