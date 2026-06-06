// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Uptime tracking (`snow/uptime`): the [`UptimeManager`] accrues each node's
//! online duration into a persisted [`UptimeState`], reading time through an
//! injected [`Clock`](ava_utils::clock::Clock), and the [`LockedCalculator`]
//! adapter gates queries until bootstrap completes.
//!
//! This subsystem is **off** the determinism-critical path: uptime feeds reward
//! accounting, not block decisions (`specs/06` §6.3), so it reads wall time and
//! uses float division like Go.
//!
//! Port of `snow/uptime/{manager,state,locked_calculator}.go`.

pub mod error;
pub mod locked;
pub mod manager;
pub mod state;

pub use error::{Error, Result};
pub use locked::LockedCalculator;
pub use manager::{Calculator, UptimeManager};
pub use state::{DbUptimeState, InjectedError, MemUptimeState, UptimeState};
