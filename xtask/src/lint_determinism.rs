// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Determinism-audit AST pass (specs/24 PART A, §A.2; tier X / X.19).
//!
//! `cargo xtask lint-determinism` walks every `.rs` under `crates/**/src/**`
//! (skipping `tests/`/`benches/`/`fuzz/`/`examples/` and `#[cfg(test)]` modules —
//! wall-clock in tests is fine) and statically enforces the auto-enforceable
//! determinism hazards of `specs/24-determinism-and-clock.md`:
//!
//! * **Hazard #5 — wall-clock reads.** Flags calls to wall-clock sources
//!   (`SystemTime::now`, `Utc::now`, `Local::now`, `chrono::Utc::now`,
//!   `chrono::Local::now`) outside the clock crate. Monotonic timers
//!   (`Instant::now`, `tokio::time::Instant::now`) are deliberately **not**
//!   flagged: latency/perf timing does not leak into consensus state. (This
//!   monotonic-vs-wall refinement of the spec's blanket wording mirrors
//!   `RealClock`, where `now()` = wall and `monotonic()` = `Instant`.)
//! * **Hazard #1 — non-deterministic maps in codec types.** Flags a
//!   `HashMap`/`HashSet`/`IndexMap`/`IndexSet` field on any struct/enum whose
//!   `#[derive(...)]` list includes the codec derive (`AvaCodec`) — these
//!   serialize in nondeterministic order.
//! * **Hazard #4 — non-vendored RNG on consensus paths.** Flags
//!   `rand::`/`thread_rng`/`StdRng`/`OsRng`/`rand::random` inside the sampler +
//!   consensus crates. Only the vendored MT19937 path is allowed there.
//! * **Hazard #8 — bare `Tau` second arithmetic.** Delegates to
//!   `scripts/tau_lint.sh` (the canonical grep guard).
//!
//! ## Allowlist
//! Findings are suppressed two ways:
//!  1. an inline `// determinism-allow: <reason>` comment on the offending line
//!     (the reason must be non-empty); and
//!  2. `xtask/determinism-allowlist.toml` entries keyed by repo-relative `file`
//!     + `symbol` (file+symbol granularity, not brittle line numbers).

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use syn::spanned::Spanned;
use syn::visit::Visit;
use walkdir::WalkDir;

/// The codec derive macro whose presence makes a type's field order wire-visible
/// (`crates/ava-codec-derive` `#[proc_macro_derive(AvaCodec, ...)]`).
const CODEC_DERIVE: &str = "AvaCodec";

/// Wall-clock entry points (hazard #5). The last path segment is matched, so both
/// `SystemTime::now()` and `std::time::SystemTime::now()` are caught; likewise
/// `Utc::now()` / `chrono::Utc::now()`.
const WALL_CLOCK_TYPES: &[&str] = &["SystemTime", "Utc", "Local"];

/// Monotonic timers that are explicitly NOT hazard #5 (latency/perf timing).
const MONOTONIC_TYPES: &[&str] = &["Instant"];

/// Non-deterministic-order collection idents (hazard #1).
const NONDET_MAP_TYPES: &[&str] = &["HashMap", "HashSet", "IndexMap", "IndexSet"];

/// Crate directory names whose consensus/sampling code may use only the vendored
/// MT19937 RNG (hazard #4). Matched against the `crates/<name>/` path segment.
const CONSENSUS_CRATES: &[&str] = &["ava-proposervm", "ava-snow", "ava-engine", "ava-validators"];

/// Forbidden RNG idents on consensus paths (hazard #4).
const FORBIDDEN_RNG_IDENTS: &[&str] = &["thread_rng", "StdRng", "OsRng", "random"];

/// Which determinism hazard a [`Finding`] belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hazard {
    /// Hazard #1 — non-deterministic map in a codec-derived type.
    CodecMap,
    /// Hazard #4 — non-vendored RNG on a consensus path.
    Rng,
    /// Hazard #5 — direct wall-clock read.
    WallClock,
}

impl Hazard {
    /// The hazard number from `specs/24` PART A.
    const fn number(self) -> u8 {
        match self {
            Hazard::CodecMap => 1,
            Hazard::Rng => 4,
            Hazard::WallClock => 5,
        }
    }
}

impl fmt::Display for Hazard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "hazard #{}", self.number())
    }
}

/// One determinism finding at a source location.
#[derive(Debug, Clone)]
pub struct Finding {
    /// Path to the offending file.
    pub file: PathBuf,
    /// 1-based line number.
    pub line: usize,
    /// Which hazard was hit.
    pub hazard: Hazard,
    /// Human-readable detail (the offending symbol / field).
    pub detail: String,
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}: {}",
            self.file.display(),
            self.line,
            self.hazard,
            self.detail
        )
    }
}

/// One `xtask/determinism-allowlist.toml` entry. File+symbol granularity is
/// preferred over line numbers (line numbers churn across edits).
#[derive(Debug, Clone, Deserialize)]
struct AllowEntry {
    /// Repo-relative path of the allowlisted file.
    file: String,
    /// The offending symbol (e.g. `SystemTime::now`, `Utc::now`) this entry
    /// suppresses within `file`. A finding's `detail` must contain this string.
    symbol: String,
    /// Justification (required, non-empty) — why this site is determinism-safe.
    #[allow(dead_code)]
    reason: String,
    /// Optional hazard restriction (`1`/`4`/`5`); if set, only that hazard is
    /// suppressed for the file+symbol pair.
    hazard: Option<u8>,
}

/// Parsed `determinism-allowlist.toml`.
#[derive(Debug, Default, Deserialize)]
struct Allowlist {
    #[serde(default)]
    allow: Vec<AllowEntry>,
}

impl Allowlist {
    /// Does any entry suppress `finding` (matched by repo-relative path suffix,
    /// symbol substring, and optional hazard)?
    fn suppresses(&self, finding: &Finding) -> bool {
        let path = finding.file.to_string_lossy();
        self.allow.iter().any(|e| {
            path.replace('\\', "/").ends_with(&e.file)
                && finding.detail.contains(&e.symbol)
                && e.hazard.is_none_or(|h| h == finding.hazard.number())
        })
    }
}

/// CLI entrypoint. `path` overrides the default workspace `crates/` scan (used by
/// the fixture test). Prints findings to stderr and returns `Err` if any survive
/// the allowlist.
pub fn run(path: Option<&Path>) -> Result<()> {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask manifest dir has no parent (cannot locate repo root)")?;

    let scan_root = match path {
        Some(p) => p.to_path_buf(),
        None => repo_root.join("crates"),
    };

    let files = rust_sources(&scan_root)?;
    let allowlist = load_allowlist(repo_root)?;

    let mut findings = scan_files(&files)?;
    findings.retain(|f| !allowlist.suppresses(f));
    findings.sort_by(|a, b| {
        (a.file.as_path(), a.line, a.hazard.number()).cmp(&(
            b.file.as_path(),
            b.line,
            b.hazard.number(),
        ))
    });

    // Hazard #8 (bare `Tau` second arithmetic) is the canonical grep in
    // `scripts/tau_lint.sh`; shell out to it so there is a single source of truth.
    let tau_ok = run_tau_lint(repo_root)?;

    for f in &findings {
        eprintln!("{f}");
    }

    if !findings.is_empty() || !tau_ok {
        bail!(
            "lint-determinism: {} unallowlisted finding(s){}",
            findings.len(),
            if tau_ok { "" } else { " + tau_lint failure" }
        );
    }
    eprintln!("lint-determinism: clean (hazards #1/#4/#5/#8).");
    Ok(())
}

/// Scan an explicit list of `.rs` files, returning every finding (allowlist NOT
/// applied — callers/tests apply [`Allowlist::suppresses`] as needed). This is
/// the hermetic seam the fixture test drives.
pub fn scan_files(files: &[PathBuf]) -> Result<Vec<Finding>> {
    let mut out = Vec::new();
    for file in files {
        let src =
            fs::read_to_string(file).with_context(|| format!("reading {}", file.display()))?;
        scan_source(file, &src, &mut out)?;
    }
    Ok(out)
}

/// Parse `src` and append all findings for `file`.
fn scan_source(file: &Path, src: &str, out: &mut Vec<Finding>) -> Result<()> {
    let ast =
        syn::parse_file(src).with_context(|| format!("parsing {} as Rust", file.display()))?;

    let allow_lines = inline_allow_lines(src);
    let in_consensus_crate = is_consensus_path(file);

    let mut visitor = DeterminismVisitor {
        file,
        src,
        in_consensus_crate,
        findings: Vec::new(),
    };
    visitor.visit_file(&ast);

    for f in visitor.findings {
        if !allow_lines.contains(&f.line) {
            out.push(f);
        }
    }
    Ok(())
}

/// Lines bearing a non-empty `// determinism-allow: <reason>` annotation.
fn inline_allow_lines(src: &str) -> BTreeSet<usize> {
    const TAG: &str = "// determinism-allow:";
    let mut set = BTreeSet::new();
    for (idx, line) in src.lines().enumerate() {
        if let Some((_, reason)) = line.split_once(TAG)
            && !reason.trim().is_empty()
        {
            set.insert(idx.saturating_add(1)); // 1-based
        }
    }
    set
}

/// Is `file` inside one of the [`CONSENSUS_CRATES`] (for hazard #4)? The sampler
/// lives in `ava-utils`, so its sampler module is matched explicitly.
fn is_consensus_path(file: &Path) -> bool {
    let p = file.to_string_lossy().replace('\\', "/");
    if CONSENSUS_CRATES
        .iter()
        .any(|c| p.contains(&format!("/{c}/")) || p.contains(&format!("{c}/src")))
    {
        return true;
    }
    // `ava-utils` sampler module only (not all of ava-utils).
    p.contains("ava-utils/src/sampler")
}

/// The syn visitor accumulating findings for one file.
struct DeterminismVisitor<'a> {
    file: &'a Path,
    src: &'a str,
    in_consensus_crate: bool,
    findings: Vec<Finding>,
}

impl DeterminismVisitor<'_> {
    /// 1-based line of a span (syn `proc-macro2` spans are 1-based already).
    fn line_of(&self, span: proc_macro2::Span) -> usize {
        let l = span.start().line;
        if l == 0 {
            // `extra-traits`/locations unavailable — fall back to line 1.
            1
        } else {
            l
        }
    }

    fn push(&mut self, line: usize, hazard: Hazard, detail: String) {
        self.findings.push(Finding {
            file: self.file.to_path_buf(),
            line,
            hazard,
            detail,
        });
    }
}

impl<'ast> Visit<'ast> for DeterminismVisitor<'_> {
    /// Skip `#[cfg(test)]` modules entirely — wall-clock in tests is fine.
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if has_cfg_test(&node.attrs) {
            return;
        }
        syn::visit::visit_item_mod(self, node);
    }

    /// Skip `#[cfg(test)]` functions (e.g. inline test helpers).
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if has_cfg_test(&node.attrs) {
            return;
        }
        syn::visit::visit_item_fn(self, node);
    }

    /// Hazard #5 (wall clock) and hazard #4 (RNG) — both surface as call exprs.
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(p) = node.func.as_ref() {
            let segs: Vec<String> = p
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect();

            // `<Type>::now(...)` — wall clock vs monotonic. Use `iter().rev()` to
            // read the last two segments without index arithmetic.
            let mut rev = segs.iter().rev();
            if let (Some(last), Some(ty)) = (rev.next(), rev.next())
                && last == "now"
                && WALL_CLOCK_TYPES.contains(&ty.as_str())
                && !MONOTONIC_TYPES.contains(&ty.as_str())
            {
                let line = self.line_of(node.span());
                self.push(line, Hazard::WallClock, format!("{ty}::now"));
            }

            // Hazard #4: `rand::random()` / `thread_rng()` / `StdRng::...` etc.
            if self.in_consensus_crate {
                self.check_rng_path(&segs, node.span());
            }
        }
        syn::visit::visit_expr_call(self, node);
    }

    /// Method-call form of a wall-clock read is rare, but `chrono::Utc::now()`
    /// resolves through `visit_expr_call`; nothing extra needed here. We still
    /// recurse for completeness.
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        syn::visit::visit_expr_method_call(self, node);
    }

    /// Hazard #1 — a non-deterministic map field on a codec-derived struct.
    fn visit_item_struct(&mut self, node: &'ast syn::ItemStruct) {
        if has_cfg_test(&node.attrs) {
            return;
        }
        if derives_codec(&node.attrs) {
            for field in &node.fields {
                if let Some(ty) = nondet_map_ident(&field.ty) {
                    let line = self.line_of(field.ty.span());
                    let name = field
                        .ident
                        .as_ref()
                        .map_or_else(|| "<tuple field>".to_string(), ToString::to_string);
                    self.push(
                        line,
                        Hazard::CodecMap,
                        format!("{ty} field `{name}` on #[derive({CODEC_DERIVE})] type"),
                    );
                }
            }
        }
        syn::visit::visit_item_struct(self, node);
    }

    /// Hazard #1 for enum variants holding a codec-derived map.
    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        if has_cfg_test(&node.attrs) {
            return;
        }
        if derives_codec(&node.attrs) {
            for variant in &node.variants {
                for field in &variant.fields {
                    if let Some(ty) = nondet_map_ident(&field.ty) {
                        let line = self.line_of(field.ty.span());
                        self.push(
                            line,
                            Hazard::CodecMap,
                            format!(
                                "{ty} in variant `{}` on #[derive({CODEC_DERIVE})] enum",
                                variant.ident
                            ),
                        );
                    }
                }
            }
        }
        syn::visit::visit_item_enum(self, node);
    }
}

impl DeterminismVisitor<'_> {
    fn check_rng_path(&mut self, segs: &[String], span: proc_macro2::Span) {
        let last = segs.last().map(String::as_str).unwrap_or_default();
        let has_rand_root = segs.first().map(String::as_str) == Some("rand");
        let forbidden_ident = FORBIDDEN_RNG_IDENTS.contains(&last)
            || segs
                .iter()
                .any(|s| FORBIDDEN_RNG_IDENTS.contains(&s.as_str()));
        if (has_rand_root && last != "Error") || forbidden_ident {
            let _ = self.src; // span carries the location; keep src for future use
            let line = self.line_of(span);
            self.push(
                line,
                Hazard::Rng,
                format!("non-vendored RNG `{}`", segs.join("::")),
            );
        }
    }
}

/// Does the attribute list carry `#[cfg(test)]`?
fn has_cfg_test(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        if !a.path().is_ident("cfg") {
            return false;
        }
        let mut found = false;
        let _ = a.parse_nested_meta(|meta| {
            if meta.path.is_ident("test") {
                found = true;
            }
            Ok(())
        });
        found
    })
}

/// Does the attribute list include `#[derive(... AvaCodec ...)]`?
fn derives_codec(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        if !a.path().is_ident("derive") {
            return false;
        }
        let mut found = false;
        let _ = a.parse_nested_meta(|meta| {
            if meta
                .path
                .segments
                .last()
                .is_some_and(|s| s.ident == CODEC_DERIVE)
            {
                found = true;
            }
            Ok(())
        });
        found
    })
}

/// If `ty` is (or wraps, e.g. `Option<HashMap<..>>`) a non-deterministic map,
/// return the map ident.
fn nondet_map_ident(ty: &syn::Type) -> Option<&'static str> {
    let syn::Type::Path(tp) = ty else {
        return None;
    };
    let seg = tp.path.segments.last()?;
    let ident = seg.ident.to_string();
    if let Some(name) = NONDET_MAP_TYPES.iter().find(|m| ***m == ident) {
        return Some(name);
    }
    // Recurse through generic args (Option<..>, Vec<..>, etc.).
    if let syn::PathArguments::AngleBracketed(args) = &seg.arguments {
        for arg in &args.args {
            if let syn::GenericArgument::Type(inner) = arg
                && let Some(found) = nondet_map_ident(inner)
            {
                return Some(found);
            }
        }
    }
    None
}

/// Collect every scannable `.rs` under `root`, skipping non-source dirs.
fn rust_sources(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        bail!("scan root does not exist: {}", root.display());
    }
    let mut files = Vec::new();
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_skipped_dir(e.path()))
    {
        let entry = entry.with_context(|| format!("walking {}", root.display()))?;
        let path = entry.path();
        if path.is_file() && path.extension().is_some_and(|e| e == "rs") {
            files.push(path.to_path_buf());
        }
    }
    files.sort();
    Ok(files)
}

/// Directories whose contents are out of scope (tests/benches/fuzz/examples and
/// build/target artifacts).
fn is_skipped_dir(path: &Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    matches!(
        path.file_name().and_then(|n| n.to_str()),
        Some("tests" | "benches" | "fuzz" | "examples" | "target")
    )
}

/// Load `xtask/determinism-allowlist.toml` (empty if absent).
fn load_allowlist(repo_root: &Path) -> Result<Allowlist> {
    let path = repo_root.join("xtask/determinism-allowlist.toml");
    if !path.exists() {
        return Ok(Allowlist::default());
    }
    let raw = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let allowlist: Allowlist =
        toml::from_str(&raw).with_context(|| format!("parsing {}", path.display()))?;
    for e in &allowlist.allow {
        if e.reason.trim().is_empty() {
            bail!(
                "determinism-allowlist.toml: empty reason for {} {}",
                e.file,
                e.symbol
            );
        }
    }
    Ok(allowlist)
}

/// Run `scripts/tau_lint.sh` (hazard #8). Returns `true` on success.
fn run_tau_lint(repo_root: &Path) -> Result<bool> {
    let script = repo_root.join("scripts/tau_lint.sh");
    if !script.exists() {
        // Tolerate a missing script (it is a no-op until ava-saevm* lands).
        return Ok(true);
    }
    let status = Command::new("bash")
        .arg(&script)
        .current_dir(repo_root)
        .status()
        .with_context(|| format!("failed to spawn `bash {}`", script.display()))?;
    Ok(status.success())
}
