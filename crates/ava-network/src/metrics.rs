// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Network-level Prometheus metrics — the `avalanche_network_*` families.
//!
//! Byte-exact port of `network/metrics.go` (§2.1) and `network/throttling/*.go`
//! (§2.3) per `specs/18-metrics-and-logging.md`. The metric *names* and *label
//! keys* are a frozen compatibility surface scraped by operator dashboards: any
//! rename/relabel here is a protocol break (`specs/18` §3).
//!
//! ## Naming convention (the `avalanche_network_` prefix)
//!
//! Following Go's registry model (`specs/18` §1.1) and the [`crate`]'s sibling
//! `ava-database::meterdb` pattern, this struct registers **bare** family names
//! (`peers`, `times_connected`, …) into the passed sub-registry. The node-level
//! `PrefixGatherer` (owned by `ava-api`, `specs/18` §1.2) rewrites each family
//! to `avalanche_network_<name>` on scrape. The golden test (`tests/metrics.rs`)
//! therefore asserts the bare names; the `avalanche_network_` prefix is verified
//! end-to-end by the node-level metrics test in a later milestone.
//!
//! ## Wiring status (M2.20)
//!
//! Registration + exact names/labels is the hard requirement of M2.20 and is
//! complete. Live increments are wired where they are low-risk; the remaining
//! call sites carry a `// metrics:` note and are left for the milestone that
//! owns the corresponding runtime surface (peer-set bookkeeping, the dial/accept
//! loops, the uptime calculator). Every metric below is registered and
//! constructible regardless.

use prometheus::{Counter, Gauge, GaugeVec, IntGauge, Opts, Registry};

use crate::error::{Error, Result};

/// The `subnetID` label carried by `peers_subnet` (`network/metrics.go`).
const SUBNET_ID_LABEL: &str = "subnetID";

/// Maps a `prometheus` error into the crate error enum.
fn to_metrics_err<E: std::fmt::Display>(e: E) -> Error {
    Error::Metrics(e.to_string())
}

/// The network-level metric set — `avalanche_network_*` (`specs/18` §2.1, §2.3).
///
/// Cheap to [`Clone`] (every field is an `Arc`-backed Prometheus handle), so the
/// `Network` can hand clones to the dialer, accept loop, peer set, and the three
/// throttlers, all metering into the same registered series.
#[derive(Clone)]
pub struct Metrics {
    // --- §2.1 network/metrics.go ---
    /// Number of network peers (`peers`).
    pub peers: IntGauge,
    /// Currently tracked IPs being connected to (`tracked`).
    pub tracked: IntGauge,
    /// Peers validating a particular subnet, by `subnetID` (`peers_subnet`).
    pub peers_subnet: GaugeVec,
    /// Nanoseconds since the last message was received (`time_since_last_msg_received`).
    pub time_since_last_msg_received: Gauge,
    /// Nanoseconds since the last message was sent (`time_since_last_msg_sent`).
    pub time_since_last_msg_sent: Gauge,
    /// Portion of recently-failed sends (`send_fail_rate`).
    pub send_fail_rate: Gauge,
    /// Completed handshakes with a peer (`times_connected`).
    pub times_connected: Counter,
    /// Disconnects after a completed handshake (`times_disconnected`).
    pub times_disconnected: Counter,
    /// Listener failed to accept an inbound connection (`accept_failed`).
    pub accept_failed: Counter,
    /// Allowed inbound connections (`inbound_conn_throttler_allowed`).
    pub inbound_conn_throttler_allowed: Counter,
    /// Connections rejected for an unsupported TLS certificate (`tls_conn_rejected`).
    pub tls_conn_rejected: Counter,
    /// Useless bytes received in `PeerList` messages (`num_useless_peerlist_bytes`).
    pub num_useless_peerlist_bytes: Counter,
    /// Inbound connections rejected by the rate-limiter (`inbound_conn_throttler_rate_limited`).
    pub inbound_conn_throttler_rate_limited: Counter,
    /// Uptime weighted by observer stake (`node_uptime_weighted_average`).
    pub node_uptime_weighted_average: Gauge,
    /// Percentage of stake deeming this node reward-eligible (`node_uptime_rewarding_stake`).
    pub node_uptime_rewarding_stake: Gauge,
    /// Average peer connection duration in nanoseconds (`peer_connected_duration_average`).
    pub peer_connected_duration_average: Gauge,

    // --- §2.3 network/throttling/*.go ---
    /// Inbound connections awaiting a bandwidth slot (`bandwidth_throttler_inbound_awaiting_acquire`).
    pub bandwidth_throttler_inbound_awaiting_acquire: IntGauge,
    /// Inbound connections awaiting a buffer slot (`buffer_throttler_inbound_awaiting_acquire`).
    pub buffer_throttler_inbound_awaiting_acquire: IntGauge,
    /// At-large inbound byte budget remaining (`byte_throttler_inbound_remaining_at_large_bytes`).
    pub byte_throttler_inbound_remaining_at_large_bytes: IntGauge,
    /// Validator inbound byte budget remaining (`byte_throttler_inbound_remaining_validator_bytes`).
    pub byte_throttler_inbound_remaining_validator_bytes: IntGauge,
    /// Inbound messages awaiting a byte acquire (`byte_throttler_inbound_awaiting_acquire`).
    pub byte_throttler_inbound_awaiting_acquire: IntGauge,
    /// Inbound messages awaiting a byte release (`byte_throttler_inbound_awaiting_release`).
    pub byte_throttler_inbound_awaiting_release: IntGauge,
    /// Inbound resource-throttler waits (`throttler_total_waits`).
    pub throttler_total_waits: Counter,
    /// Inbound resource-throttler acquires without waiting (`throttler_total_no_waits`).
    pub throttler_total_no_waits: Counter,
    /// Inbound messages awaiting a CPU-resource acquire (`throttler_awaiting_acquire`).
    pub throttler_awaiting_acquire: IntGauge,
    /// Outbound acquire successes (`throttler_outbound_acquire_successes`).
    pub throttler_outbound_acquire_successes: Counter,
    /// Outbound acquire failures (`throttler_outbound_acquire_failures`).
    pub throttler_outbound_acquire_failures: Counter,
    /// Outbound at-large byte budget remaining (`throttler_outbound_remaining_at_large_bytes`).
    pub throttler_outbound_remaining_at_large_bytes: IntGauge,
    /// Outbound validator byte budget remaining (`throttler_outbound_remaining_validator_bytes`).
    pub throttler_outbound_remaining_validator_bytes: IntGauge,
    /// Outbound messages awaiting a byte release (`throttler_outbound_awaiting_release`).
    pub throttler_outbound_awaiting_release: IntGauge,
}

impl Metrics {
    /// Registers every `avalanche_network_*` family against `reg` (bare names;
    /// the node `PrefixGatherer` adds the `avalanche_network_` prefix). Errors
    /// with [`Error::Metrics`] on a registration failure (e.g. duplicate name).
    pub fn new(reg: &Registry) -> Result<Self> {
        // --- §2.1 ---
        let peers = IntGauge::new("peers", "number of network peers").map_err(to_metrics_err)?;
        let tracked = IntGauge::new("tracked", "currently tracked IPs being connected to")
            .map_err(to_metrics_err)?;
        let peers_subnet = GaugeVec::new(
            Opts::new("peers_subnet", "peers validating a particular subnet"),
            &[SUBNET_ID_LABEL],
        )
        .map_err(to_metrics_err)?;
        let time_since_last_msg_received = Gauge::new(
            "time_since_last_msg_received",
            "time (in ns) since the last msg was received",
        )
        .map_err(to_metrics_err)?;
        let time_since_last_msg_sent = Gauge::new(
            "time_since_last_msg_sent",
            "time (in ns) since the last msg was sent",
        )
        .map_err(to_metrics_err)?;
        let send_fail_rate = Gauge::new(
            "send_fail_rate",
            "portion of messages that recently failed to be sent over the network",
        )
        .map_err(to_metrics_err)?;
        let times_connected = Counter::new(
            "times_connected",
            "times this node successfully completed a handshake with a peer",
        )
        .map_err(to_metrics_err)?;
        let times_disconnected = Counter::new(
            "times_disconnected",
            "times this node disconnected from a peer it had completed a handshake with",
        )
        .map_err(to_metrics_err)?;
        let accept_failed = Counter::new(
            "accept_failed",
            "times this node's listener failed to accept an inbound connection",
        )
        .map_err(to_metrics_err)?;
        let inbound_conn_throttler_allowed = Counter::new(
            "inbound_conn_throttler_allowed",
            "times this node allowed an inbound connection through the inbound connection throttler",
        )
        .map_err(to_metrics_err)?;
        let tls_conn_rejected = Counter::new(
            "tls_conn_rejected",
            "times this node rejected a connection due to an unsupported TLS certificate",
        )
        .map_err(to_metrics_err)?;
        let num_useless_peerlist_bytes = Counter::new(
            "num_useless_peerlist_bytes",
            "amount of useless bytes (i.e. information about peers we already knew/don't want to connect to) received in PeerList messages",
        )
        .map_err(to_metrics_err)?;
        let inbound_conn_throttler_rate_limited = Counter::new(
            "inbound_conn_throttler_rate_limited",
            "times this node rejected an inbound connection due to rate-limiting",
        )
        .map_err(to_metrics_err)?;
        let node_uptime_weighted_average = Gauge::new(
            "node_uptime_weighted_average",
            "this node's uptime average weighted by observing peer stakes",
        )
        .map_err(to_metrics_err)?;
        let node_uptime_rewarding_stake = Gauge::new(
            "node_uptime_rewarding_stake",
            "the percentage of total stake which thinks this node is eligible for rewards",
        )
        .map_err(to_metrics_err)?;
        let peer_connected_duration_average = Gauge::new(
            "peer_connected_duration_average",
            "average duration of all peer connections in nanoseconds",
        )
        .map_err(to_metrics_err)?;

        // --- §2.3 throttling ---
        let bandwidth_throttler_inbound_awaiting_acquire = IntGauge::new(
            "bandwidth_throttler_inbound_awaiting_acquire",
            "number of inbound connections that are awaiting acquiring bandwidth",
        )
        .map_err(to_metrics_err)?;
        let buffer_throttler_inbound_awaiting_acquire = IntGauge::new(
            "buffer_throttler_inbound_awaiting_acquire",
            "number of inbound messages waiting to acquire a buffer slot",
        )
        .map_err(to_metrics_err)?;
        let byte_throttler_inbound_remaining_at_large_bytes = IntGauge::new(
            "byte_throttler_inbound_remaining_at_large_bytes",
            "number of bytes remaining in the at-large byte allocation",
        )
        .map_err(to_metrics_err)?;
        let byte_throttler_inbound_remaining_validator_bytes = IntGauge::new(
            "byte_throttler_inbound_remaining_validator_bytes",
            "number of bytes remaining in the validator byte allocation",
        )
        .map_err(to_metrics_err)?;
        let byte_throttler_inbound_awaiting_acquire = IntGauge::new(
            "byte_throttler_inbound_awaiting_acquire",
            "number of inbound messages waiting to acquire bytes",
        )
        .map_err(to_metrics_err)?;
        let byte_throttler_inbound_awaiting_release = IntGauge::new(
            "byte_throttler_inbound_awaiting_release",
            "number of inbound messages waiting to release bytes",
        )
        .map_err(to_metrics_err)?;
        let throttler_total_waits = Counter::new(
            "throttler_total_waits",
            "number of times an inbound message was throttled by the inbound resource throttler",
        )
        .map_err(to_metrics_err)?;
        let throttler_total_no_waits = Counter::new(
            "throttler_total_no_waits",
            "number of times an inbound message was immediately allowed by the inbound resource throttler",
        )
        .map_err(to_metrics_err)?;
        let throttler_awaiting_acquire = IntGauge::new(
            "throttler_awaiting_acquire",
            "number of inbound messages waiting to acquire a CPU-resource slot",
        )
        .map_err(to_metrics_err)?;
        let throttler_outbound_acquire_successes = Counter::new(
            "throttler_outbound_acquire_successes",
            "number of times the outbound message throttler successfully acquired bytes",
        )
        .map_err(to_metrics_err)?;
        let throttler_outbound_acquire_failures = Counter::new(
            "throttler_outbound_acquire_failures",
            "number of times the outbound message throttler failed to acquire bytes",
        )
        .map_err(to_metrics_err)?;
        let throttler_outbound_remaining_at_large_bytes = IntGauge::new(
            "throttler_outbound_remaining_at_large_bytes",
            "number of bytes remaining in the outbound at-large byte allocation",
        )
        .map_err(to_metrics_err)?;
        let throttler_outbound_remaining_validator_bytes = IntGauge::new(
            "throttler_outbound_remaining_validator_bytes",
            "number of bytes remaining in the outbound validator byte allocation",
        )
        .map_err(to_metrics_err)?;
        let throttler_outbound_awaiting_release = IntGauge::new(
            "throttler_outbound_awaiting_release",
            "number of outbound messages waiting to release bytes",
        )
        .map_err(to_metrics_err)?;

        let m = Self {
            peers,
            tracked,
            peers_subnet,
            time_since_last_msg_received,
            time_since_last_msg_sent,
            send_fail_rate,
            times_connected,
            times_disconnected,
            accept_failed,
            inbound_conn_throttler_allowed,
            tls_conn_rejected,
            num_useless_peerlist_bytes,
            inbound_conn_throttler_rate_limited,
            node_uptime_weighted_average,
            node_uptime_rewarding_stake,
            peer_connected_duration_average,
            bandwidth_throttler_inbound_awaiting_acquire,
            buffer_throttler_inbound_awaiting_acquire,
            byte_throttler_inbound_remaining_at_large_bytes,
            byte_throttler_inbound_remaining_validator_bytes,
            byte_throttler_inbound_awaiting_acquire,
            byte_throttler_inbound_awaiting_release,
            throttler_total_waits,
            throttler_total_no_waits,
            throttler_awaiting_acquire,
            throttler_outbound_acquire_successes,
            throttler_outbound_acquire_failures,
            throttler_outbound_remaining_at_large_bytes,
            throttler_outbound_remaining_validator_bytes,
            throttler_outbound_awaiting_release,
        };
        m.register(reg)?;
        Ok(m)
    }

    /// Registers every collector against `reg` (mirrors Go's
    /// `errors.Join(reg.Register(...))`).
    fn register(&self, reg: &Registry) -> Result<()> {
        macro_rules! reg {
            ($field:expr) => {
                reg.register(Box::new($field.clone()))
                    .map_err(to_metrics_err)?;
            };
        }
        reg!(self.peers);
        reg!(self.tracked);
        reg!(self.peers_subnet);
        reg!(self.time_since_last_msg_received);
        reg!(self.time_since_last_msg_sent);
        reg!(self.send_fail_rate);
        reg!(self.times_connected);
        reg!(self.times_disconnected);
        reg!(self.accept_failed);
        reg!(self.inbound_conn_throttler_allowed);
        reg!(self.tls_conn_rejected);
        reg!(self.num_useless_peerlist_bytes);
        reg!(self.inbound_conn_throttler_rate_limited);
        reg!(self.node_uptime_weighted_average);
        reg!(self.node_uptime_rewarding_stake);
        reg!(self.peer_connected_duration_average);
        reg!(self.bandwidth_throttler_inbound_awaiting_acquire);
        reg!(self.buffer_throttler_inbound_awaiting_acquire);
        reg!(self.byte_throttler_inbound_remaining_at_large_bytes);
        reg!(self.byte_throttler_inbound_remaining_validator_bytes);
        reg!(self.byte_throttler_inbound_awaiting_acquire);
        reg!(self.byte_throttler_inbound_awaiting_release);
        reg!(self.throttler_total_waits);
        reg!(self.throttler_total_no_waits);
        reg!(self.throttler_awaiting_acquire);
        reg!(self.throttler_outbound_acquire_successes);
        reg!(self.throttler_outbound_acquire_failures);
        reg!(self.throttler_outbound_remaining_at_large_bytes);
        reg!(self.throttler_outbound_remaining_validator_bytes);
        reg!(self.throttler_outbound_awaiting_release);
        Ok(())
    }

    /// Records a completed handshake (Go `network.Connected`).
    ///
    /// metrics: wired here so the `Network` increments on every successful
    /// handshake completion; the `peers` gauge is set by the peer set.
    pub fn observe_connected(&self) {
        self.times_connected.inc();
    }

    /// Records a post-handshake disconnect (Go `network.Disconnected`).
    pub fn observe_disconnected(&self) {
        self.times_disconnected.inc();
    }

    /// Records a connection rejected for an unsupported TLS certificate.
    ///
    /// metrics: wired at the upgrader/verifier reject path when the rustls
    /// verifier callback is threaded a `Metrics` handle (a later milestone);
    /// callers that already detect the reject increment this directly.
    pub fn observe_tls_conn_rejected(&self) {
        self.tls_conn_rejected.inc();
    }

    /// Adds `bytes` useless `PeerList` bytes (Go `numUselessPeerListBytes.Add`).
    ///
    /// metrics: wired at the `PeerList` gossip-ingest site when the IP tracker
    /// reports newly-useless bytes; registration is unconditional.
    pub fn add_useless_peerlist_bytes(&self, bytes: f64) {
        self.num_useless_peerlist_bytes.inc_by(bytes);
    }

    /// Sets the inbound byte-throttler "remaining" gauges from the throttler's
    /// current pool state. Low-risk: the caller already holds the counts, so no
    /// throttler refactor is required to push them here.
    pub fn set_inbound_byte_remaining(&self, at_large: i64, validator: i64) {
        self.byte_throttler_inbound_remaining_at_large_bytes
            .set(at_large);
        self.byte_throttler_inbound_remaining_validator_bytes
            .set(validator);
    }

    /// Touches one series per labelled family so a fresh registry materialises
    /// it on `gather()` (used by the parity test only).
    #[doc(hidden)]
    pub fn touch_for_test(&self) {
        self.peers_subnet.with_label_values(&["0"]).set(0.0);
    }
}
