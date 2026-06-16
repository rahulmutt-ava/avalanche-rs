// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! PORTING.md matrix aggregation (specs/02 §10.1, §13; tier X / X.20, M9.23).
//!
//! Walks every `tests/PORTING.md` under `crates/*/tests/` and `tests/*/tests/`,
//! parses the status cell of each matrix row, counts the per-crate
//! `✅ / 🟡 / ⬜ / n/a` totals, and detects any `| wip ` table row. The
//! `porting-report` task prints a per-crate + grand-total report and fails
//! (non-zero) if **any** crate still carries a `wip` row — the PORTING-matrix
//! half of the M9.23 acceptance gate (specs/02 §10.1: "zero `wip` rows").
//!
//! Only lines that are table rows (start with `|`) are scanned, so prose /
//! legend lines that merely *name* "wip" or the status glyphs never trip the
//! gate. The row-scanning + count logic is modelled on `saevm_exit_gate.rs`.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Per-`PORTING.md` parsed status-cell tallies + any forbidden `wip` rows.
#[derive(Default)]
pub struct PortingCounts {
    /// `✅` (ported) status cells.
    pub ported: usize,
    /// `🟡` (partial) status cells.
    pub partial: usize,
    /// `⬜` (not-ported) status cells — a legitimate documented deferral.
    pub not_ported: usize,
    /// `n/a` (not-applicable) status cells.
    pub na: usize,
    /// `1`-based line numbers of any `| wip ` table rows (must be empty for the
    /// acceptance gate to pass).
    pub wip_lines: Vec<usize>,
}

/// One parsed `tests/PORTING.md` and its counts.
pub struct PortingFile {
    /// Repo-relative path (for the report + failure messages).
    pub rel: String,
    /// Parsed status-cell tallies + `wip`-row line numbers.
    pub counts: PortingCounts,
}

/// The repo root (the xtask manifest dir's parent).
///
/// # Errors
/// Fails if the xtask manifest dir has no parent (impossible in a valid
/// workspace layout).
pub fn repo_root() -> anyhow::Result<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .context("xtask manifest dir has no parent (cannot locate repo root)")?
        .to_path_buf())
}

/// Discover every `tests/PORTING.md` under `crates/*/tests/` and
/// `tests/*/tests/`, returning them sorted by relative path for a deterministic
/// report.
///
/// # Errors
/// Returns an error if a `PORTING.md` is discovered but cannot be read.
pub fn collect(root: &Path) -> anyhow::Result<Vec<PortingFile>> {
    let mut found: Vec<PathBuf> = Vec::new();
    // `crates/*/tests/PORTING.md` (the crate-level matrices) and
    // `tests/*/tests/PORTING.md` (suite matrices: reexecute / load / upgrade).
    for top in ["crates", "tests"] {
        let base = root.join(top);
        let Ok(entries) = fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join("tests").join("PORTING.md");
            if candidate.is_file() {
                found.push(candidate);
            }
        }
    }
    found.sort();

    let mut out = Vec::with_capacity(found.len());
    for path in found {
        let src = fs::read_to_string(&path)
            .with_context(|| format!("reading PORTING.md at {}", path.display()))?;
        let rel = path
            .strip_prefix(root)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| path.display().to_string());
        out.push(PortingFile {
            rel,
            counts: parse(&src),
        });
    }
    Ok(out)
}

/// Parse one `PORTING.md` body: tally status cells and record `wip`-row line
/// numbers. Only table rows (lines whose first non-whitespace char is `|`) are
/// scanned; header (`| Go test`/`| Exit test`/`| Target`) and separator
/// (`|---`) rows are skipped.
#[must_use]
pub fn parse(src: &str) -> PortingCounts {
    let mut c = PortingCounts::default();
    for (lineno, line) in src.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('|') {
            continue;
        }
        // A `| wip ` table row is forbidden by the acceptance gate.
        if line.contains("| wip ") {
            c.wip_lines.push(lineno.saturating_add(1));
        }
        // Skip header / separator rows; only data rows carry a status glyph.
        if trimmed.starts_with("| Go test")
            || trimmed.starts_with("| Exit test")
            || trimmed.starts_with("| Target")
            || trimmed.starts_with("|---")
        {
            continue;
        }
        // Status is the second pipe-delimited cell (cells[0] == "" before the
        // first pipe, cells[1] == name, cells[2] == status).
        let cells: Vec<&str> = trimmed.split('|').map(str::trim).collect();
        let Some(status) = cells.get(2) else {
            continue;
        };
        match *status {
            "✅" | "✅ ported" => c.ported = c.ported.saturating_add(1),
            "🟡" | "🟡 partial" => c.partial = c.partial.saturating_add(1),
            "⬜" | "⬜ not ported" => c.not_ported = c.not_ported.saturating_add(1),
            "n/a" => c.na = c.na.saturating_add(1),
            _ => {}
        }
    }
    c
}

/// `porting-report`: aggregate every crate's `tests/PORTING.md` into one report
/// with per-matrix `✅ / 🟡 / ⬜ / n/a` counts + a grand total, and fail
/// (non-zero) if any matrix still carries a `wip` row (specs/02 §10.1, M9.23).
///
/// # Errors
/// Returns an error if a discovered `PORTING.md` cannot be read, or if any
/// matrix carries one or more `| wip ` table rows.
pub fn report() -> anyhow::Result<()> {
    let root = repo_root()?;
    let files = collect(&root)?;

    let mut out = String::new();
    out.push_str("PORTING.md matrix aggregation (specs/02 §10.1):\n\n");

    let (mut t_ported, mut t_partial, mut t_not, mut t_na) = (0usize, 0usize, 0usize, 0usize);
    let mut wip_offenders: Vec<String> = Vec::new();

    for f in &files {
        let c = &f.counts;
        t_ported = t_ported.saturating_add(c.ported);
        t_partial = t_partial.saturating_add(c.partial);
        t_not = t_not.saturating_add(c.not_ported);
        t_na = t_na.saturating_add(c.na);

        let wip_note = if c.wip_lines.is_empty() {
            String::new()
        } else {
            let lines = c
                .wip_lines
                .iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join(", ");
            wip_offenders.push(format!("{} (lines {lines})", f.rel));
            format!("  <-- {} WIP ROW(S) @ lines {lines}", c.wip_lines.len())
        };
        let _ = writeln!(
            out,
            "  {:<48} {:>3} ✅ / {:>2} 🟡 / {:>2} ⬜ / {:>2} n/a{wip_note}",
            f.rel, c.ported, c.partial, c.not_ported, c.na,
        );
    }

    let _ = writeln!(
        out,
        "\n  {:<48} {:>3} ✅ / {:>2} 🟡 / {:>2} ⬜ / {:>2} n/a  ({} matrices)",
        "TOTAL",
        t_ported,
        t_partial,
        t_not,
        t_na,
        files.len(),
    );
    print!("{out}");

    if wip_offenders.is_empty() {
        println!(
            "\nporting-report: zero `wip` rows across {} matrices.",
            files.len()
        );
        Ok(())
    } else {
        bail!(
            "porting-report: {} matrix/matrices still carry `wip` rows:\n  - {}",
            wip_offenders.len(),
            wip_offenders.join("\n  - "),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_counts_status_cells() {
        let src = "\
| Go test | Status | note |
|---|---|---|
| `TestA` | ✅ ported | done |
| `TestB` | 🟡 partial | wip-ish prose only, not a status |
| `TestC` | n/a | go-specific |
| `TestD` | ⬜ not ported | deferred |
";
        let c = parse(src);
        assert_eq!(c.ported, 1, "parse() ported count");
        assert_eq!(c.partial, 1, "parse() partial count");
        assert_eq!(c.na, 1, "parse() n/a count");
        assert_eq!(c.not_ported, 1, "parse() not-ported count");
        assert!(c.wip_lines.is_empty(), "parse() must not flag prose 'wip'");
    }

    #[test]
    fn parse_flags_wip_table_rows_only() {
        let src = "\
Legend: ⬜ not ported · 🟡 partial · wip = in progress
| Go test | Status | note |
|---|---|---|
| `TestX` | wip | a real wip row |
| `TestY` | ✅ ported | done |
";
        let c = parse(src);
        // The legend prose line names `wip` but is not a table row -> not flagged.
        // Only the `| wip ` table row (line 4, 1-based) is flagged.
        assert_eq!(
            c.wip_lines,
            vec![4],
            "parse() flags only `| wip ` table rows"
        );
        assert_eq!(c.ported, 1, "parse() still counts the ✅ row");
    }

    #[test]
    fn parse_skips_header_and_separator() {
        let src = "| Exit test | File | Oracle |\n|---|---|---|\n";
        let c = parse(src);
        assert_eq!(c.ported, 0, "header/separator rows carry no status");
        assert_eq!(c.partial, 0, "header/separator rows carry no status");
    }
}
