// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The [`Engine`] trait — `Handler` plus `start`/`health_check` (port of
//! `snow/engine/common.Engine`, specs 06 §4.1).

use async_trait::async_trait;

use crate::common::handler::Handler;
use crate::error::Result;

/// `snow/engine/common.Engine` — a consensus engine: the full inbound-op
/// [`Handler`] plus lifecycle (`Start`) and the health checker.
///
/// All node IDs are assumed pre-authenticated. An engine may recover after
/// returning an error, but it is not required to.
#[async_trait]
pub trait Engine: Handler {
    /// `Start` — begin engine operations from the given request ID.
    async fn start(&mut self, start_req_id: u32) -> Result<()>;

    /// `health.Checker.HealthCheck` — returns engine health detail as a JSON
    /// value (periodically polled and reported through the health API). `Err`
    /// indicates an unhealthy engine.
    fn health_check(&self) -> Result<serde_json::Value>;
}
