// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The Snowman engine family (port of `snow/engine/snowman/`, specs 06 §4.2–§4.4).
//!
//! * [`engine`] — the normal-operation [`SnowmanEngine`](engine::SnowmanEngine)
//!   (issue / poll / vote loop).
//! * [`poll`] — the outstanding-[`PollSet`](poll::PollSet) with the
//!   [`EarlyTermFactory`](poll::EarlyTermFactory) early-termination predicate.
//! * [`getter`] — the read-only [`Getter`](getter::Getter) server side.
//! * [`issuer`] / [`voter`] — doc modules mapping the Go job machinery onto the
//!   inline engine flow.
//!
//! M3.12 adds `bootstrap` (the bootstrapper state machine + interval tree +
//! height-ordered acceptor) and `syncer` (the state-sync skeleton).

pub mod adaptor;
pub mod engine;
pub mod getter;
pub mod issuer;
pub mod poll;
pub mod voter;

pub use adaptor::BlockAdaptor;
pub use engine::{Config, SnowmanEngine};
pub use getter::Getter;
pub use poll::{EarlyTermFactory, Poll, PollSet};
