// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Golden-vector corpus management (specs/22 §2.2, §6; tier X / X.10–X.12).

use clap::Subcommand;

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
///
/// SCAFFOLD: the verify/diff/regen logic + the `ava-testvectors` loader are
/// owned by tier-X tasks X.10/X.11/X.12. The corpus and manifest already exist
/// under `tests/vectors/`; this entrypoint is wired so CI and contributors can
/// discover the surface.
pub fn run(action: Action) -> anyhow::Result<()> {
    let what = match action {
        Action::Verify => "verify",
        Action::Diff { .. } => "diff",
        Action::Regen => "regen",
    };
    eprintln!(
        "xtask vectors {what}: corpus verify/diff/regen is owned by tier-X tasks X.10–X.12 \
         (manifest + schema live under tests/vectors/)."
    );
    Ok(())
}
