// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The embedded rpcchainvm-protocol compatibility table (`compatibility.json`).
//!
//! Mirrors `version.RPCChainVMProtocolCompatibility`, loaded in Go via
//! `//go:embed compatibility.json` (`version/compatibility.go`). The table maps
//! each rpcchainvm **protocol version** (decimal string key in the JSON) to the
//! set of `avalanchego` releases that shipped that protocol version.
//!
//! **What it is and is NOT.** As the Go comment states, *"This is not used by
//! avalanchego, but is useful for downstream libraries."* It is a lookup table
//! for VM authors / tooling ("which node releases can host my plugin built
//! against protocol 44?") — it is **NOT** consulted in the peer connect/reject
//! path (that is the numeric rule in [`crate::compatibility`], `specs/26` §3).
//! We reproduce it because `info`/admin tooling and the plugin-management story
//! surface it, and a golden test pins it byte-for-byte against the Go file
//! (`specs/26` §9(6), `specs/00` §5).
//!
//! Source: copied verbatim from the Go tree's `version/compatibility.json`
//! (provenance in `compatibility.json.md`). It is *data, not a generated
//! artifact*, so it is checked in. Bumping the node version updates both
//! `application.rs` and this file in the same change — exactly as Go bumps
//! `constants.go` + `compatibility.json` together.
//!
//! Owning spec: `specs/26-versioning-and-compatibility.md` §4.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::error::{Error, Result};

/// The embedded compatibility table JSON, byte-identical to the Go tree's
/// `version/compatibility.json` (see `compatibility.json.md` for provenance).
pub static COMPATIBILITY_JSON: &str = include_str!("../compatibility.json");

/// The parsed rpcchainvm-protocol compatibility table, computed once.
///
/// Map: rpcchainvm protocol version → the `avalanchego` releases that
/// implemented it. Lookup/tooling only; NOT used for peer accept/reject.
///
/// Wrapping the parse result in a `LazyLock<Result<..>>` keeps library code
/// panic-free (no `unwrap`/`expect`): the embedded file is byte-parity-tested,
/// so the `Err` arm is unreachable in practice but is surfaced rather than
/// panicking. Use [`rpc_chain_vm_protocol_compatibility`] for an owned copy.
static TABLE: LazyLock<Result<BTreeMap<u32, Vec<String>>>> = LazyLock::new(parse_table);

/// Parses [`COMPATIBILITY_JSON`] into the protocol → versions table.
///
/// Mirrors how Go decodes `RPCChainVMProtocolCompatibility` from the embedded
/// JSON. The JSON keys are decimal strings (Go uses string keys); we re-key to
/// `u32` to match [`crate::RPC_CHAIN_VM_PROTOCOL`]'s type.
fn parse_table() -> Result<BTreeMap<u32, Vec<String>>> {
    let raw: BTreeMap<String, Vec<String>> = serde_json::from_str(COMPATIBILITY_JSON)
        .map_err(|e| Error::CompatibilityTable(e.to_string()))?;
    raw.into_iter()
        .map(|(k, v)| {
            let key = k.parse::<u32>().map_err(|e| {
                Error::CompatibilityTable(format!("non-decimal protocol key {k:?}: {e}"))
            })?;
            Ok((key, v))
        })
        .collect()
}

/// Returns the rpcchainvm-protocol compatibility table (protocol → versions).
///
/// Mirrors `version.RPCChainVMProtocolCompatibility`. Returns
/// [`Error::CompatibilityTable`] only if the embedded `compatibility.json`
/// fails to parse — which the byte-parity golden test guards against.
pub fn rpc_chain_vm_protocol_compatibility() -> Result<BTreeMap<u32, Vec<String>>> {
    match &*TABLE {
        Ok(table) => Ok(table.clone()),
        Err(e) => Err(e.clone()),
    }
}
