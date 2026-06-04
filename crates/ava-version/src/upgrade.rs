// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `UpgradeConfig` + `Fork` + the network-upgrade activation schedule.
//!
//! TODO(M0.23): per `specs/03-core-primitives.md` §5.2 + §11.2 — `UpgradeConfig`
//! with all phase times (`DateTime<Utc>`) + the three non-time side params
//! (`apricot_phase_4_min_p_chain_height: u64`, `cortina_x_chain_stop_vertex_id:
//! Id`, `granite_epoch_duration: Duration`). `Fork` enum (15 time-gated phases,
//! `Ord` = chronological) + `Fork::ALL`; `fork_time`, `is_active` (`t >=
//! fork_time`), `fork_at`, `validate` (15 time fields monotonic non-decreasing).
//! `get_config(network_id)` -> Mainnet/Fuji/Default with VERBATIM constants.
