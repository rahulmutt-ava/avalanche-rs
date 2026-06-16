// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-load` — the sustained-load test harness (specs/02 §10.3; specs/16 §5
//! perf; specs/00 §7.3 metric-name parity; M9.18).
//!
//! Skeleton crate registered by the M9.18 prep commit. The load generator
//! ([`generator`]), Prometheus scraper + SLO assertions, and the offline /
//! gated-live arm split are filled in by task M9.18.
