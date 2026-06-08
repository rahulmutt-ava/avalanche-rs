# Upgrading the pinned reth revision (G0 / R3)

`ava-evm-reth` is the **G0 facade**: the only crate in the workspace allowed to
name `reth_*` / `revm` / `alloy_*` directly (spec 10 §17.1, 00 §11.1.6). It pins
reth to **one git revision** so the rest of `ava-evm` (and, in M7,
`ava-saevm-exec`) depend only on this crate's re-exports. A reth bump is a
localized, mechanical edit — the blast radius is this one crate.

## Currently pinned

| Dep | Source | Pin |
|---|---|---|
| `reth-*` | git `paradigmxyz/reth` | `v2.2.0` = `88505c7fcbfdebfd3b56d88c86b62e950043c6c4` |
| `revm` | crates.io | `38.0.0` |
| `revm-inspectors` | crates.io | `0.39.0` |
| `alloy-primitives` | crates.io | `1.5.6` |
| `alloy-consensus` | crates.io | `2.0.4` |
| `alloy-evm` | crates.io | `0.34.0` |
| `alloy-rlp` | crates.io | `0.3.13` |

> reth pins `revm` / `alloy-*` **by version** in its own workspace (not git), so
> we mirror those exact versions here. They must stay in lockstep with the reth
> rev: read them from `reth/Cargo.toml` at the pinned rev when bumping.

## Bump checklist

1. **Move the SHA.** Pick a new reth tag/commit. Update every `reth-*` `rev=` in
   `Cargo.toml` **and** the `RETH_REV` const in `src/lib.rs` to the new SHA.
2. **Re-mirror `revm`/`alloy-*` versions.** Read them from `reth/Cargo.toml` at
   the new rev (`revm`, `alloy-primitives`, `alloy-consensus`, `alloy-evm`,
   `alloy-rlp`) and update this crate's `Cargo.toml` + the table above.
3. **Fix compile errors ONLY inside this crate.** Upstream renames/moves surface
   as broken re-exports here (and as a failing `tests/facade_pins.rs`). Repair
   the re-export paths and the trait impls; do not touch `ava-evm`.
4. **Re-run the differential gate** (spec 10 §14): `differential::cchain_state_root`
   is the acceptance test — state-root parity vs Go must hold across the bump.

## Why git-rev, not crates.io, for reth

reth's library crates (`reth-evm`, `reth-provider`/`reth-storage-api`,
`reth-rpc`, `reth-transaction-pool`, `reth-chainspec`, …) carry **no semver
stability guarantee** for the SDK trait set we consume, and reth ships no
first-class "external-consensus executor" entrypoint. Pinning one rev makes the
whole set coherent and the upgrade explicit.
