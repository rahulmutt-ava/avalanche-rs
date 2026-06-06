// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! [`LockedCalculator`] — the bootstrap-gated [`Calculator`] adapter
//! (port of `snow/uptime/locked_calculator.go`).
//!
//! Until a backing calculator is installed via [`LockedCalculator::set_calculator`]
//! every query returns [`Error::StillBootstrapping`]. Once installed, calls
//! forward to the inner calculator.
//!
//! Go layers two locks: an `Atomic[bool]` + `RWMutex` guarding the slot, and a
//! `sync.Locker` (`calculatorLock`) held across the inner call. We mirror that
//! split: a `std::sync::Mutex` guards the (swappable) slot, and a
//! `tokio::sync::Mutex` serializes the inner queries (Go's `calculatorLock`).
//! The `None`/`Some` slot state encodes "still bootstrapping" vs "ready" — Go
//! sets the bootstrap flag and the calculator together.

use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime};

use tokio::sync::Mutex as AsyncMutex;

use ava_types::node_id::NodeId;

use super::error::{Error, Result};
use super::manager::Calculator;

/// Bootstrap-gated, mutex-serialized [`Calculator`] (Go `lockedCalculator`).
#[derive(Default)]
pub struct LockedCalculator {
    /// The installable calculator slot (Go `c` + bootstrap flag).
    slot: StdMutex<Option<Arc<dyn Calculator>>>,
    /// Serializes inner queries (Go `calculatorLock`).
    query_lock: AsyncMutex<()>,
}

impl LockedCalculator {
    /// Creates a calculator that reports [`Error::StillBootstrapping`] until a
    /// backing calculator is installed (Go `NewLockedCalculator`).
    #[must_use]
    pub fn new() -> Self {
        Self {
            slot: StdMutex::new(None),
            query_lock: AsyncMutex::new(()),
        }
    }

    /// Installs (or replaces) the backing calculator (Go `SetCalculator`).
    pub fn set_calculator(&self, calculator: Arc<dyn Calculator>) {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = Some(calculator);
        }
    }

    /// Clears the backing calculator, returning to the bootstrapping state.
    pub fn clear(&self) {
        if let Ok(mut slot) = self.slot.lock() {
            *slot = None;
        }
    }

    fn calculator(&self) -> Result<Arc<dyn Calculator>> {
        self.slot
            .lock()
            .ok()
            .and_then(|slot| slot.clone())
            .ok_or(Error::StillBootstrapping)
    }

    /// See [`Calculator::calculate_uptime`]; gated on bootstrap.
    ///
    /// # Errors
    /// [`Error::StillBootstrapping`] before a calculator is installed; otherwise
    /// the inner calculator's error.
    pub async fn calculate_uptime(&self, node_id: NodeId) -> Result<(Duration, SystemTime)> {
        let calculator = self.calculator()?;
        let _guard = self.query_lock.lock().await;
        calculator.calculate_uptime(node_id)
    }

    /// See [`Calculator::calculate_uptime_percent`]; gated on bootstrap.
    ///
    /// # Errors
    /// [`Error::StillBootstrapping`] before a calculator is installed; otherwise
    /// the inner calculator's error.
    pub async fn calculate_uptime_percent(&self, node_id: NodeId) -> Result<f64> {
        let calculator = self.calculator()?;
        let _guard = self.query_lock.lock().await;
        calculator.calculate_uptime_percent(node_id)
    }

    /// See [`Calculator::calculate_uptime_percent_from`]; gated on bootstrap.
    ///
    /// # Errors
    /// [`Error::StillBootstrapping`] before a calculator is installed; otherwise
    /// the inner calculator's error.
    pub async fn calculate_uptime_percent_from(
        &self,
        node_id: NodeId,
        start_time: SystemTime,
    ) -> Result<f64> {
        let calculator = self.calculator()?;
        let _guard = self.query_lock.lock().await;
        calculator.calculate_uptime_percent_from(node_id, start_time)
    }
}
