// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `cargo xtask` — the repo automation task surface (specs/01 §5, tier X / X.4).
//!
//! Mirrors the canonical [`Taskfile.yml`] task names so contributors and CI can
//! drive cross-cutting workflows (testing, golden-vector management, the
//! differential harness, the PORTING.md matrices, and the determinism audit)
//! through one entrypoint. Subcommands whose deep logic lands in later tier-X
//! tasks are wired here as thin shells that exit non-zero with a pointer to the
//! owning task, so the surface is complete and discoverable from M0.
//!
//! [`Taskfile.yml`]: ../../Taskfile.yml

#![forbid(unsafe_code)]

mod lint_determinism;
mod porting;
mod test;
mod vectors;

use clap::{Parser, Subcommand};

/// avalanche-rs repo automation (mirrors Taskfile.yml; specs/01 §5).
#[derive(Parser)]
#[command(name = "xtask", about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run unit tests (nextest, CI profile) + doctests.
    TestUnit,
    /// Fast local unit tests (no all-features, no checks profile).
    TestUnitFast,
    /// Run cargo-fuzz targets briefly (smoke).
    TestFuzz,
    /// Run the differential harness against the recorded Go oracle / live nodes.
    TestDifferential {
        /// Replay a single program by seed.
        #[arg(long)]
        seed: Option<u64>,
        /// Recorded-oracle mode (replay vs Go-recorded outputs).
        #[arg(long)]
        recorded: bool,
    },
    /// Reexecute golden block ranges and compare state roots.
    TestReexecute,
    /// Manage the golden-vector corpus (verify / diff / regen).
    Vectors {
        #[command(subcommand)]
        action: vectors::Action,
    },
    /// Aggregate the per-crate PORTING.md matrices into one report.
    PortingReport,
    /// Determinism-audit AST pass (wall-clock / HashMap-in-codec / RNG bans).
    LintDeterminism,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::TestUnit => test::test_unit(),
        Command::TestUnitFast => test::test_unit_fast(),
        Command::TestFuzz => test::test_fuzz(),
        Command::TestDifferential { seed, recorded } => test::test_differential(seed, recorded),
        Command::TestReexecute => test::test_reexecute(),
        Command::Vectors { action } => vectors::run(action),
        Command::PortingReport => porting::report(),
        Command::LintDeterminism => lint_determinism::run(),
    }
}
