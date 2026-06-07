// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The block-issuer job (port of `snow/engine/snowman/issuer.go`, specs 06 §4.2).
//!
//! In Go, an `issuer` is a `job.Job[ids.ID]` parked in the engine's `blocked`
//! scheduler until its parent block has been issued, at which point it calls
//! `deliver` to verify the block and add it to consensus.
//!
//! In this port the dependency graph is resolved eagerly inside
//! [`SnowmanEngine::issue_from`](crate::snowman::engine::SnowmanEngine::issue_from)
//! because the engine task is single-owner and the VM/consensus calls are
//! `await`ed inline (see the module note in [`engine`](crate::snowman::engine)).
//! This module documents the correspondence and exists so the file layout mirrors
//! the Go tree; the issuance state machine itself lives in `engine.rs`:
//!
//! | Go `issuer` step            | Rust location                              |
//! |-----------------------------|--------------------------------------------|
//! | `Execute` → `deliver`       | `SnowmanEngine::issue_from`                |
//! | verify + `Consensus.Add`    | `add_unverified_block_to_consensus`        |
//! | `SetPreference` + query     | inline in `issue_from`                     |
//! | abandon on bad parent       | `should_issue_block` / `can_issue_child_on`|
