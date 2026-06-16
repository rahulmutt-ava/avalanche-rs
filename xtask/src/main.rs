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

mod acceptance;
mod bench_guard;
mod check_sae_lints;
mod gen_flags;
mod gen_genesis;
mod lint_determinism;
mod porting;
mod saevm_exit_gate;
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
    TestFuzz {
        /// Run each target for an extended duration instead of a brief smoke.
        #[arg(long)]
        long: bool,
    },
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
    /// Sustained-load suite: tx stream + metric-name SLOs (specs/02 §10.3, M9.18).
    TestLoad,
    /// Rolling-upgrade suite: Go→Rust across an activation height (specs/02 §10.4, M9.17).
    TestUpgrade,
    /// Manage the golden-vector corpus (verify / diff / regen).
    Vectors {
        #[command(subcommand)]
        action: vectors::Action,
    },
    /// Aggregate the per-crate PORTING.md matrices into one report.
    PortingReport,
    /// Determinism-audit AST pass (wall-clock / HashMap-in-codec / RNG bans).
    LintDeterminism,
    /// Structural guard for the SAE stricter-lint bar (forbid-unsafe / pedantic
    /// / arithmetic_side_effects on the ava-saevm crates).
    CheckSaeLints,
    /// Regenerate the Go flag-catalog snapshot for golden::flag_parity
    /// (crates/ava-config/tests/vectors/config/flags.json; specs/13 §25).
    GenFlags,
    /// M7 (SAE VM / ACP-194) milestone exit gate: assert the named exit tests
    /// exist, PORTING.md is complete (no wip/placeholder rows), and the golden
    /// vectors + fuzz target are present (specs/11 §10 + exit gate).
    SaevmExitGate,
    /// Re-freeze the ava-genesis golden vectors from the Go oracle
    /// (`genesis.FromConfig` byte dumps + golden IDs; specs 23 §7, M8.8).
    GenGenesis,
    /// Run the critical-path criterion benches and fail on a >threshold
    /// regression vs the committed baselines (specs/02 §9, 16 §5(9), 00 §9).
    BenchGuard {
        /// Regression threshold as a fraction (default 0.10 == 10%).
        #[arg(long)]
        threshold: Option<f64>,
    },
    /// M9.23 final acceptance gate (specs/16 §5 definition of done): assert every
    /// `16` §5 DoD item maps to a named exit test that exists (offline + live
    /// arms) and that every crate's `tests/PORTING.md` has zero `wip` rows.
    Acceptance,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::TestUnit => test::test_unit(),
        Command::TestUnitFast => test::test_unit_fast(),
        Command::TestFuzz { long } => test::test_fuzz(long),
        Command::TestDifferential { seed, recorded } => test::test_differential(seed, recorded),
        Command::TestReexecute => test::test_reexecute(),
        Command::TestLoad => test::test_load(),
        Command::TestUpgrade => test::test_upgrade(),
        Command::Vectors { action } => vectors::run(action),
        Command::PortingReport => porting::report(),
        Command::LintDeterminism => lint_determinism::run(),
        Command::CheckSaeLints => check_sae_lints::run(),
        Command::GenFlags => gen_flags::run(),
        Command::SaevmExitGate => saevm_exit_gate::run(),
        Command::GenGenesis => gen_genesis::run(),
        Command::BenchGuard { threshold } => bench_guard::run(threshold),
        Command::Acceptance => acceptance::run(),
    }
}
