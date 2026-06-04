// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Determinism-audit AST pass (specs/24 PART A, §A.2; tier X / X.19).

/// `lint-determinism`: ban wall-clock outside `ava-utils::clock`, `HashMap`/
/// `HashSet` in codec-derive types, non-vendored RNG on consensus paths, and
/// bare `Tau` second arithmetic — the nine determinism hazards of specs/24.
///
/// SCAFFOLD: the `syn`-based AST pass + allowlist (`determinism-allowlist.toml`)
/// are owned by tier-X task X.19. The clippy-level guards (`float_arithmetic`,
/// `arithmetic_side_effects`) and the `tau_lint.sh` grep already provide partial
/// coverage from M0.
pub fn run() -> anyhow::Result<()> {
    eprintln!(
        "xtask lint-determinism: the AST determinism pass is owned by tier-X task X.19 \
         (clippy guards + scripts/tau_lint.sh provide partial coverage)."
    );
    Ok(())
}
