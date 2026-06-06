// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! VM health checking (`api/health.Checker`, specs 07 §2.2).

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::error::Result;

/// `api/health.Checker` — periodically polled and reported via the node's Health
/// API.
///
/// A healthy VM returns `Ok(json)` where the JSON is the (marshallable) health
/// detail Go returns as `interface{}`; an unhealthy VM returns `Err`. Go's
/// `context.Context` becomes a `&CancellationToken`.
#[async_trait]
pub trait HealthCheck: Send + Sync {
    /// Returns the health-check result. `Ok(value)` when healthy (the value is
    /// surfaced as JSON via the Health API); `Err` when unhealthy.
    async fn health_check(&self, token: &CancellationToken) -> Result<serde_json::Value>;
}
