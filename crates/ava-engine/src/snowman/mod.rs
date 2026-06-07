// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Snowman engine family (port of `snow/engine/snowman/`, specs 06 §4.2–§4.4).
//!
//! * [`engine`] — the normal-operation [`SnowmanEngine`](engine::SnowmanEngine)
//!   (issue / poll / vote loop).
//! * [`poll`] — the outstanding-[`PollSet`](poll::PollSet) with the
//!   [`EarlyTermFactory`](poll::EarlyTermFactory) early-termination predicate.
//! * [`getter`] — the read-only [`Getter`](getter::Getter) server side.
//! * [`bootstrap`] — the [`Bootstrapper`](bootstrap::Bootstrapper) state machine
//!   + interval tree + height-ordered acceptor.
//! * [`syncer`] — the state-sync skeleton (no-op state-summary handlers).
//! * [`issuer`] / [`voter`] — doc modules mapping the Go job machinery onto the
//!   inline engine flow.

pub mod adaptor;
pub mod bootstrap;
pub mod engine;
pub mod getter;
pub mod issuer;
pub mod poll;
pub mod syncer;
pub mod voter;

pub use adaptor::BlockAdaptor;
pub use bootstrap::Bootstrapper;
pub use engine::{Config, SnowmanEngine};
pub use getter::Getter;
pub use poll::{EarlyTermFactory, Poll, PollSet};
pub use syncer::StateSyncer;
