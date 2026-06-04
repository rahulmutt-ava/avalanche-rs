// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! PORTING.md matrix aggregation (specs/02 §10.1; tier X / X.20).

/// `porting-report`: aggregate every crate's `tests/PORTING.md` into one report
/// with per-subsystem percent-ported.
///
/// SCAFFOLD: the aggregation + `wip`-blocks-done check are owned by tier-X task
/// X.20. Per-crate `tests/PORTING.md` files already exist (seeded in M0).
pub fn report() -> anyhow::Result<()> {
    eprintln!(
        "xtask porting-report: matrix aggregation is owned by tier-X task X.20 \
         (per-crate tests/PORTING.md already seeded)."
    );
    Ok(())
}
