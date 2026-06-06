// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-peer message I/O metrics — registered into the shared `avalanche_network`
//! registerer (`specs/18` §2.2, `network/peer/metrics.go`).
//!
//! Byte-exact family names and label keys: `msgs`/`msgs_bytes` carry
//! `io`,`op`,`compressed`; `msgs_bytes_saved` carries `io`,`op`;
//! `msgs_failed_to_send` carries `op`. The `round_trip` and `clock_skew`
//! averagers expand to `<name>_count` + `<name>_sum` counter pairs (`specs/18`
//! §2 averager note). These are scraped by dashboards: a rename is a protocol
//! break (`specs/18` §3).
//!
//! ## Wiring status (M2.20 → M2.20b)
//!
//! Registration + exact names/labels is complete. M2.20b threaded a
//! `PeerMetrics` handle through [`PeerConfig`](crate::config::PeerConfig)
//! (`with_peer_metrics`) into the peer read/write tasks:
//!
//! - the read task (`peer::peer::handle_inbound`) calls
//!   [`PeerMetrics::observe_received`] on a parsed frame and
//!   [`PeerMetrics::observe_failed_to_parse`] on a parse error;
//! - the write task (`peer::peer::write_frame`) calls [`PeerMetrics::observe_sent`]
//!   on a successful write and [`PeerMetrics::observe_failed_to_send`] on a
//!   write error.
//!
//! The handle is `Option`-typed in `PeerConfig`, so a peer built without a
//! metrics registry simply no-ops the observations. Registration is
//! unconditional.

use prometheus::{Counter, CounterVec, Opts, Registry};

use ava_message::ops::Op;

use crate::error::{Error, Result};

/// `io` label key (`network/peer/metrics.go`).
const IO_LABEL: &str = "io";
/// `op` label key.
const OP_LABEL: &str = "op";
/// `compressed` label key.
const COMPRESSED_LABEL: &str = "compressed";

/// `io="sent"` label value.
pub const IO_SENT: &str = "sent";
/// `io="received"` label value.
pub const IO_RECEIVED: &str = "received";

/// Renders a `compressed` boolean as Go's `strconv.FormatBool` does
/// (`"true"`/`"false"`), matching `network/peer/metrics.go`.
fn compressed_str(compressed: bool) -> &'static str {
    if compressed { "true" } else { "false" }
}

/// Maps a `prometheus` error into the crate error enum.
fn to_metrics_err<E: std::fmt::Display>(e: E) -> Error {
    Error::Metrics(e.to_string())
}

/// An averager — Go `metric.NewAveragerWithErrs(name, …)` registers a
/// `<name>_count` (Counter) and `<name>_sum` (Counter) pair (`specs/18` §2).
#[derive(Clone)]
struct Averager {
    count: Counter,
    sum: Counter,
}

impl Averager {
    fn new(reg: &Registry, name: &str, help: &str) -> Result<Self> {
        let count = Counter::new(
            format!("{name}_count"),
            format!("# of observations of {help}"),
        )
        .map_err(to_metrics_err)?;
        let sum = Counter::new(format!("{name}_sum"), format!("sum of {help}"))
            .map_err(to_metrics_err)?;
        reg.register(Box::new(count.clone()))
            .map_err(to_metrics_err)?;
        reg.register(Box::new(sum.clone()))
            .map_err(to_metrics_err)?;
        Ok(Self { count, sum })
    }

    /// Observes one sample of `value` (Go `Averager.Observe`).
    fn observe(&self, value: f64) {
        self.count.inc();
        self.sum.inc_by(value);
    }
}

/// Per-peer message I/O metrics (`specs/18` §2.2). Cheap to [`Clone`]; one set
/// is registered into the network registerer and shared across all peers (Go
/// registers these once in `network/peer/metrics.go` and every `peer` shares
/// them).
#[derive(Clone)]
pub struct PeerMetrics {
    /// Ping/pong round-trip time averager (`round_trip_count`/`round_trip_sum`).
    round_trip: Averager,
    /// Observed peer clock skew averager (`clock_skew_count`/`clock_skew_sum`).
    clock_skew: Averager,
    /// Received messages that failed to parse (`msgs_failed_to_parse`).
    pub msgs_failed_to_parse: Counter,
    /// Messages that failed to send, by `op` (`msgs_failed_to_send`).
    pub msgs_failed_to_send: CounterVec,
    /// Messages sent/received, by `io`,`op`,`compressed` (`msgs`).
    pub msgs: CounterVec,
    /// Bytes on the wire, by `io`,`op`,`compressed` (`msgs_bytes`).
    pub msgs_bytes: CounterVec,
    /// Bytes saved by compression, by `io`,`op` (`msgs_bytes_saved`).
    pub msgs_bytes_saved: CounterVec,
}

impl PeerMetrics {
    /// Registers every per-peer family against `reg` (bare names; the node
    /// `PrefixGatherer` adds the `avalanche_network_` prefix). Errors with
    /// [`Error::Metrics`] on a registration failure.
    pub fn new(reg: &Registry) -> Result<Self> {
        let round_trip = Averager::new(reg, "round_trip", "round trip time (ns)")?;
        let clock_skew = Averager::new(reg, "clock_skew", "observed peer clock skew (ns)")?;

        let msgs_failed_to_parse = Counter::new(
            "msgs_failed_to_parse",
            "number of received messages that failed to be parsed",
        )
        .map_err(to_metrics_err)?;
        let msgs_failed_to_send = CounterVec::new(
            Opts::new(
                "msgs_failed_to_send",
                "number of messages that failed to be sent",
            ),
            &[OP_LABEL],
        )
        .map_err(to_metrics_err)?;
        let msgs = CounterVec::new(
            Opts::new("msgs", "number of messages sent/received"),
            &[IO_LABEL, OP_LABEL, COMPRESSED_LABEL],
        )
        .map_err(to_metrics_err)?;
        let msgs_bytes = CounterVec::new(
            Opts::new(
                "msgs_bytes",
                "number of message bytes sent/received on the wire",
            ),
            &[IO_LABEL, OP_LABEL, COMPRESSED_LABEL],
        )
        .map_err(to_metrics_err)?;
        let msgs_bytes_saved = CounterVec::new(
            Opts::new(
                "msgs_bytes_saved",
                "number of message bytes saved by compression",
            ),
            &[IO_LABEL, OP_LABEL],
        )
        .map_err(to_metrics_err)?;

        reg.register(Box::new(msgs_failed_to_parse.clone()))
            .map_err(to_metrics_err)?;
        reg.register(Box::new(msgs_failed_to_send.clone()))
            .map_err(to_metrics_err)?;
        reg.register(Box::new(msgs.clone()))
            .map_err(to_metrics_err)?;
        reg.register(Box::new(msgs_bytes.clone()))
            .map_err(to_metrics_err)?;
        reg.register(Box::new(msgs_bytes_saved.clone()))
            .map_err(to_metrics_err)?;

        Ok(Self {
            round_trip,
            clock_skew,
            msgs_failed_to_parse,
            msgs_failed_to_send,
            msgs,
            msgs_bytes,
            msgs_bytes_saved,
        })
    }

    /// Observes a ping/pong round-trip time in nanoseconds.
    pub fn observe_round_trip(&self, ns: f64) {
        self.round_trip.observe(ns);
    }

    /// Observes the peer's clock skew in nanoseconds.
    pub fn observe_clock_skew(&self, ns: f64) {
        self.clock_skew.observe(ns);
    }

    /// Records a successfully-sent message of `op` carrying `wire_bytes` on the
    /// wire and `saved_bytes` saved by compression.
    ///
    /// metrics (M2.20b): called from `peer::peer::write_frame` on a successful
    /// frame write (the handshake and every queued message).
    pub fn observe_sent(&self, op: Op, compressed: bool, wire_bytes: f64, saved_bytes: f64) {
        self.observe(IO_SENT, op, compressed, wire_bytes, saved_bytes);
    }

    /// Records a successfully-received message of `op`.
    ///
    /// metrics (M2.20b): called from `peer::peer::handle_inbound` on a
    /// successfully-parsed inbound frame.
    pub fn observe_received(&self, op: Op, compressed: bool, wire_bytes: f64, saved_bytes: f64) {
        self.observe(IO_RECEIVED, op, compressed, wire_bytes, saved_bytes);
    }

    /// Records a received message that failed to parse (`msgs_failed_to_parse`).
    pub fn observe_failed_to_parse(&self) {
        self.msgs_failed_to_parse.inc();
    }

    /// Records a message of `op` that failed to send (`msgs_failed_to_send`).
    pub fn observe_failed_to_send(&self, op: Op) {
        self.msgs_failed_to_send
            .with_label_values(&[op.as_str()])
            .inc();
    }

    /// Shared sent/received accounting (Go `peer.Metrics.{Sent,Received}`).
    fn observe(&self, io: &str, op: Op, compressed: bool, wire_bytes: f64, saved_bytes: f64) {
        let op_name = op.as_str();
        let compressed_name = compressed_str(compressed);
        self.msgs
            .with_label_values(&[io, op_name, compressed_name])
            .inc();
        self.msgs_bytes
            .with_label_values(&[io, op_name, compressed_name])
            .inc_by(wire_bytes);
        if saved_bytes != 0.0 {
            self.msgs_bytes_saved
                .with_label_values(&[io, op_name])
                .inc_by(saved_bytes);
        }
    }

    /// Touches one series per labelled family so a fresh registry materialises
    /// it on `gather()` (used by the parity test only).
    #[doc(hidden)]
    pub fn touch_for_test(&self) {
        self.observe_sent(Op::Ping, false, 0.0, 0.0);
        self.observe_failed_to_send(Op::Ping);
        // msgs_bytes_saved only materialises when saved != 0.
        self.msgs_bytes_saved
            .with_label_values(&[IO_SENT, Op::Ping.as_str()])
            .inc_by(0.0);
    }
}
