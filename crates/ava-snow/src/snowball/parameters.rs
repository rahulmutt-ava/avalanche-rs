// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Snowball [`Parameters`] + `verify()` (specs 06 §2.1; Go
//! `snow/consensus/snowball/parameters.go`).
//!
//! Defaults and the validity predicate match Go bit-for-bit (they are
//! network-tuning values agreed across the network).

use std::time::Duration;

use crate::error::Error;

/// Safety buffer for `min_percent_connected_healthy` (health only). Range
/// `[0,1]`. `0` means `min_percent_connected = alpha/k`; `1` means fully
/// connected (Go `MinPercentConnectedBuffer`).
pub const MIN_PERCENT_CONNECTED_BUFFER: f64 = 0.2;

/// Parameters required for snowball consensus. Integer thresholds only
/// (mirrors `snowball.Parameters`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Parameters {
    /// Nodes sampled per poll.
    pub k: u32,
    /// Vote threshold to change preference (slush).
    pub alpha_preference: u32,
    /// Vote threshold to increment a confidence counter (snowflake).
    pub alpha_confidence: u32,
    /// Consecutive successful polls required to finalize.
    pub beta: u32,
    /// Target number of outstanding polls while work is processing.
    pub concurrent_repolls: u32,
    /// Soft cap used to throttle block building.
    pub optimal_processing: u32,
    /// Health: unhealthy if more than this many items are outstanding.
    pub max_outstanding_items: u32,
    /// Health: unhealthy if an item processes longer than this.
    pub max_item_processing_time: Duration,
}

/// The default consensus parameters (Go `DefaultParameters`).
pub const DEFAULT_PARAMETERS: Parameters = Parameters {
    k: 20,
    alpha_preference: 15,
    alpha_confidence: 15,
    beta: 20,
    concurrent_repolls: 4,
    optimal_processing: 10,
    max_outstanding_items: 256,
    max_item_processing_time: Duration::from_secs(30),
};

impl Parameters {
    /// Returns `Ok(())` if the parameters describe a valid initialization.
    ///
    /// An initialization is valid if:
    /// - `k/2 < alpha_preference <= alpha_confidence <= k`
    /// - `0 < concurrent_repolls <= beta`
    /// - `0 < optimal_processing`
    /// - `0 < max_outstanding_items`
    /// - `0 < max_item_processing_time`
    ///
    /// The branch order and the per-branch failure are a bit-for-bit port of Go
    /// `Parameters.Verify`. `k/2` is integer division, matching Go's `p.K/2`.
    /// `k/2 < k` implies `0 <= k/2`, so there is no explicit positivity check on
    /// `alpha_preference`.
    ///
    /// # Errors
    /// Returns [`Error::ParametersInvalid`] (with the failing Go condition text)
    /// when any invariant is violated.
    pub fn verify(&self) -> Result<(), Error> {
        if self.alpha_preference <= self.k / 2 {
            Err(Error::ParametersInvalid(format!(
                "k = {}, alphaPreference = {}: fails the condition that: k/2 < alphaPreference",
                self.k, self.alpha_preference
            )))
        } else if self.alpha_confidence < self.alpha_preference {
            Err(Error::ParametersInvalid(format!(
                "alphaPreference = {}, alphaConfidence = {}: fails the condition that: alphaPreference <= alphaConfidence",
                self.alpha_preference, self.alpha_confidence
            )))
        } else if self.k < self.alpha_confidence {
            Err(Error::ParametersInvalid(format!(
                "k = {}, alphaConfidence = {}: fails the condition that: alphaConfidence <= k",
                self.k, self.alpha_confidence
            )))
        } else if self.alpha_confidence == 3 && self.alpha_preference == 28 {
            // Go easter-egg branch: an unreachable-ordering guard kept for
            // behavioral parity (Go parameters.go:101). The ASCII-art message
            // body is not load-bearing; the failure (and its variant) is.
            Err(Error::ParametersInvalid(format!(
                "alphaConfidence = {}, alphaPreference = {}: fails the condition that: alphaPreference <= alphaConfidence",
                self.alpha_confidence, self.alpha_preference
            )))
        } else if self.concurrent_repolls == 0 {
            Err(Error::ParametersInvalid(format!(
                "concurrentRepolls = {}: fails the condition that: 0 < concurrentRepolls",
                self.concurrent_repolls
            )))
        } else if self.concurrent_repolls > self.beta {
            Err(Error::ParametersInvalid(format!(
                "concurrentRepolls = {}, beta = {}: fails the condition that: concurrentRepolls <= beta",
                self.concurrent_repolls, self.beta
            )))
        } else if self.optimal_processing == 0 {
            Err(Error::ParametersInvalid(format!(
                "optimalProcessing = {}: fails the condition that: 0 < optimalProcessing",
                self.optimal_processing
            )))
        } else if self.max_outstanding_items == 0 {
            Err(Error::ParametersInvalid(format!(
                "maxOutstandingItems = {}: fails the condition that: 0 < maxOutstandingItems",
                self.max_outstanding_items
            )))
        } else if self.max_item_processing_time.is_zero() {
            Err(Error::ParametersInvalid(
                "maxItemProcessingTime = 0: fails the condition that: 0 < maxItemProcessingTime"
                    .to_string(),
            ))
        } else {
            Ok(())
        }
    }

    /// The minimum fraction of connected stake required to report healthy
    /// (Go `MinPercentConnectedHealthy`). Health-only; uses floating point off
    /// the consensus decision path.
    // Non-consensus local metric: a health-check threshold, never hashed into a
    // block/vote/decision (spec 24 §B.3 / hazard #2).
    #[allow(clippy::float_arithmetic)]
    #[must_use]
    pub fn min_percent_connected_healthy(&self) -> f64 {
        // alpha_confidence is used (not alpha_preference) so the node can still
        // feasibly accept operations while healthy.
        let alpha_ratio = f64::from(self.alpha_confidence) / f64::from(self.k);
        alpha_ratio * (1.0 - MIN_PERCENT_CONNECTED_BUFFER) + MIN_PERCENT_CONNECTED_BUFFER
    }
}
