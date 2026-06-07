// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The chit-voter job (port of `snow/engine/snowman/voter.go`, specs 06 §4.2).
//!
//! In Go, a `voter` is a `job.Job[ids.ID]` parked in the engine's `blocked`
//! scheduler until the chit's referenced blocks are issued. On execution it
//! bubbles the vote to the nearest processing ancestor
//! (`getProcessingAncestor`), registers it with the poll set, and — once a poll
//! completes — feeds the resulting `Bag<Id>` into `Consensus.RecordPoll`, then
//! `VM.SetPreference(consensus.Preference())`, repolling while `NumProcessing > 0`.
//!
//! In this port that flow runs inline in
//! [`SnowmanEngine::chits`](crate::snowman::engine::SnowmanEngine::chits) /
//! [`process_poll_results`](crate::snowman::engine::SnowmanEngine) (see the module
//! note in [`engine`](crate::snowman::engine)). This module documents the
//! correspondence:
//!
//! | Go `voter` step              | Rust location                              |
//! |------------------------------|--------------------------------------------|
//! | bubble via `responseOptions` | `SnowmanEngine::apply_vote`                |
//! | `getProcessingAncestor`      | `SnowmanEngine::get_processing_ancestor`   |
//! | `polls.Vote` / `polls.Drop`  | `SnowmanEngine::chits` / `query_failed`    |
//! | `Consensus.RecordPoll`       | `SnowmanEngine::process_poll_results`      |
//! | `SetPreference` + repoll     | `process_poll_results`                     |
