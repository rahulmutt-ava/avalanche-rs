// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The `snow/engine/common` surface: the inbound-op state-machine traits
//! ([`Handler`](handler::Handler)/[`Engine`](engine::Engine)), the log-and-drop
//! [`NoOpHandler`](no_ops::NoOpHandler) default, the typed
//! [`AppError`](error::AppError), and the engine-facing
//! [`Sender`](sender::Sender) (specs 06 §4.1, §5.3).

pub mod engine;
pub mod error;
pub mod handler;
pub mod no_ops;
pub mod sender;
