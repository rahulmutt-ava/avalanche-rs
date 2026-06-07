// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The networking glue between `ava-network` (05) and the consensus engines
//! (specs 06 §5): the process-wide [`ChainRouter`], the per-chain
//! [`ChainHandler`] actor (one tokio task owning consensus state), the
//! [`AdaptiveTimeoutManager`], the [`Benchlist`], and the [`ResourceTracker`] /
//! [`Targeter`].

pub mod benchlist;
pub mod handler;
pub mod message_queue;
pub mod router;
pub mod timeout;
pub mod tracker;

pub use benchlist::{Benchlist, BenchlistConfig};
pub use handler::{
    ChainEngine, ChainHandler, ChainHandlerSink, EngineManager, HandlerMessage,
    SYNC_PROCESSING_TIME_WARN_LIMIT,
};
pub use message_queue::{MessageClass, MessageQueue};
pub use router::{ChainMessageSink, ChainRouter, InboundMessage, InboundOp, Router};
pub use timeout::{
    AdaptiveTimeoutConfig, AdaptiveTimeoutManager, RequestId, TimeoutError, TimeoutHandler,
};
pub use tracker::{CumulativeTracker, ResourceTracker, Targeter, TargeterConfig};
