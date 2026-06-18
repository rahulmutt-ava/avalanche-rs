// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `InitializeRequest.network_upgrades` wire conversion: the fork-activation
//! schedule [`UpgradeConfig`] ⇄ the proto [`NetworkUpgrades`] message (specs 07
//! §5.2).
//!
//! This is a byte-faithful port of the Go pair
//! `vms/rpcchainvm/vm_client.go:getNetworkUpgrades` (encode, host side) and
//! `vms/rpcchainvm/vm_server.go:convertNetworkUpgrades` (decode, guest side).
//! Each of the 15 time-gated forks maps to a `google.protobuf.Timestamp`
//! (`grpcutils.TimestampFromTime` = `timestamppb.New`); the three non-time
//! side-params map to a `uint64` (`apricot_phase_4_min_p_chain_height`), a 32-byte
//! `bytes` id (`cortina_x_chain_stop_vertex_id`), and a `google.protobuf.Duration`
//! (`granite_epoch_duration`).
//!
//! Before this conversion the host sent `network_upgrades = None` and the guest
//! reconstructed the schedule from `network_id`. That is unsound across the
//! Rust-host→Go-guest boundary (M9.12): Go's `convertNetworkUpgrades` rejects a
//! nil message with `errNilNetworkUpgradesPB`, so a Go guest cannot initialize
//! against a `None`. Sending the structured schedule is the wire contract.

use chrono::{DateTime, TimeZone, Utc};

use ava_types::id::Id;
use ava_version::upgrade::UpgradeConfig;

use crate::pb::vm::NetworkUpgrades;

/// Encodes a [`DateTime<Utc>`] as a proto `Timestamp` (Go
/// `grpcutils.TimestampFromTime` = `timestamppb.New`). The fork times in an
/// [`UpgradeConfig`] are whole-second instants, so `nanos` is `0` in practice;
/// the sub-second component is preserved for generality. `nanos` is always in
/// `[0, 2e9)` (chrono's leap-second range), well within `i32`, so the
/// `try_from` never truncates.
fn timestamp_from_time(t: DateTime<Utc>) -> prost_types::Timestamp {
    prost_types::Timestamp {
        seconds: t.timestamp(),
        nanos: i32::try_from(t.timestamp_subsec_nanos()).unwrap_or(0),
    }
}

/// Decodes a required proto `Timestamp` field into a [`DateTime<Utc>`] (Go
/// `grpcutils.TimestampAsTime` = `ts.CheckValid()` then `ts.AsTime()`).
///
/// `field` names the proto field for the error message. Mirrors Go's behavior of
/// failing the whole `Initialize` when any timestamp is nil or out of range.
fn time_from_timestamp(
    ts: Option<&prost_types::Timestamp>,
    field: &str,
) -> Result<DateTime<Utc>, String> {
    let ts = ts.ok_or_else(|| format!("network upgrades: {field} timestamp is nil"))?;
    let nanos =
        u32::try_from(ts.nanos).map_err(|_| format!("network upgrades: {field} negative nanos"))?;
    Utc.timestamp_opt(ts.seconds, nanos)
        .single()
        .ok_or_else(|| format!("network upgrades: {field} invalid timestamp"))
}

/// Encodes a [`std::time::Duration`] as a proto `Duration` (Go `durationpb.New`).
fn duration_from_std(d: std::time::Duration) -> prost_types::Duration {
    prost_types::Duration {
        seconds: i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
        nanos: i32::try_from(d.subsec_nanos()).unwrap_or(0),
    }
}

/// Decodes a proto `Duration` into a [`std::time::Duration`] (Go `.AsDuration()`).
/// A nil/negative-component duration maps to zero (the side-param is advisory and
/// never negative in a real config).
fn duration_to_std(d: Option<&prost_types::Duration>) -> std::time::Duration {
    let Some(d) = d else {
        return std::time::Duration::ZERO;
    };
    let secs = u64::try_from(d.seconds).unwrap_or(0);
    let nanos = u32::try_from(d.nanos).unwrap_or(0);
    std::time::Duration::new(secs, nanos)
}

/// Encodes an [`UpgradeConfig`] as the proto [`NetworkUpgrades`] message
/// (`vm_client.go:getNetworkUpgrades`).
#[must_use]
pub fn upgrades_to_proto(u: &UpgradeConfig) -> NetworkUpgrades {
    NetworkUpgrades {
        apricot_phase_1_time: Some(timestamp_from_time(u.apricot_phase_1_time)),
        apricot_phase_2_time: Some(timestamp_from_time(u.apricot_phase_2_time)),
        apricot_phase_3_time: Some(timestamp_from_time(u.apricot_phase_3_time)),
        apricot_phase_4_time: Some(timestamp_from_time(u.apricot_phase_4_time)),
        apricot_phase_4_min_p_chain_height: u.apricot_phase_4_min_p_chain_height,
        apricot_phase_5_time: Some(timestamp_from_time(u.apricot_phase_5_time)),
        apricot_phase_pre_6_time: Some(timestamp_from_time(u.apricot_phase_pre_6_time)),
        apricot_phase_6_time: Some(timestamp_from_time(u.apricot_phase_6_time)),
        apricot_phase_post_6_time: Some(timestamp_from_time(u.apricot_phase_post_6_time)),
        banff_time: Some(timestamp_from_time(u.banff_time)),
        cortina_time: Some(timestamp_from_time(u.cortina_time)),
        cortina_x_chain_stop_vertex_id: bytes::Bytes::copy_from_slice(
            &u.cortina_x_chain_stop_vertex_id.to_bytes(),
        ),
        durango_time: Some(timestamp_from_time(u.durango_time)),
        etna_time: Some(timestamp_from_time(u.etna_time)),
        fortuna_time: Some(timestamp_from_time(u.fortuna_time)),
        granite_time: Some(timestamp_from_time(u.granite_time)),
        granite_epoch_duration: Some(duration_from_std(u.granite_epoch_duration)),
        helicon_time: Some(timestamp_from_time(u.helicon_time)),
    }
}

/// Decodes a proto [`NetworkUpgrades`] message into an [`UpgradeConfig`]
/// (`vm_server.go:convertNetworkUpgrades`).
///
/// # Errors
/// Returns an error string if any time field is nil/out-of-range or the
/// `cortina_x_chain_stop_vertex_id` is not exactly 32 bytes (mirrors Go's
/// per-field `TimestampAsTime` / `ids.ToID` failures, which abort `Initialize`).
pub fn upgrades_from_proto(pb: &NetworkUpgrades) -> Result<UpgradeConfig, String> {
    Ok(UpgradeConfig {
        apricot_phase_1_time: time_from_timestamp(
            pb.apricot_phase_1_time.as_ref(),
            "apricot_phase_1_time",
        )?,
        apricot_phase_2_time: time_from_timestamp(
            pb.apricot_phase_2_time.as_ref(),
            "apricot_phase_2_time",
        )?,
        apricot_phase_3_time: time_from_timestamp(
            pb.apricot_phase_3_time.as_ref(),
            "apricot_phase_3_time",
        )?,
        apricot_phase_4_time: time_from_timestamp(
            pb.apricot_phase_4_time.as_ref(),
            "apricot_phase_4_time",
        )?,
        apricot_phase_4_min_p_chain_height: pb.apricot_phase_4_min_p_chain_height,
        apricot_phase_5_time: time_from_timestamp(
            pb.apricot_phase_5_time.as_ref(),
            "apricot_phase_5_time",
        )?,
        apricot_phase_pre_6_time: time_from_timestamp(
            pb.apricot_phase_pre_6_time.as_ref(),
            "apricot_phase_pre_6_time",
        )?,
        apricot_phase_6_time: time_from_timestamp(
            pb.apricot_phase_6_time.as_ref(),
            "apricot_phase_6_time",
        )?,
        apricot_phase_post_6_time: time_from_timestamp(
            pb.apricot_phase_post_6_time.as_ref(),
            "apricot_phase_post_6_time",
        )?,
        banff_time: time_from_timestamp(pb.banff_time.as_ref(), "banff_time")?,
        cortina_time: time_from_timestamp(pb.cortina_time.as_ref(), "cortina_time")?,
        cortina_x_chain_stop_vertex_id: Id::from_slice(&pb.cortina_x_chain_stop_vertex_id)
            .map_err(|e| format!("network upgrades: cortina_x_chain_stop_vertex_id: {e}"))?,
        durango_time: time_from_timestamp(pb.durango_time.as_ref(), "durango_time")?,
        etna_time: time_from_timestamp(pb.etna_time.as_ref(), "etna_time")?,
        fortuna_time: time_from_timestamp(pb.fortuna_time.as_ref(), "fortuna_time")?,
        granite_time: time_from_timestamp(pb.granite_time.as_ref(), "granite_time")?,
        granite_epoch_duration: duration_to_std(pb.granite_epoch_duration.as_ref()),
        helicon_time: time_from_timestamp(pb.helicon_time.as_ref(), "helicon_time")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ava_version::upgrade::get_config;

    // The mainnet, fuji, and local schedules cover the distinct shapes: real
    // historical fork times, a different min-P-chain-height, distinct non-empty
    // stop-vertex ids (mainnet/fuji) vs the empty id (local), and the unscheduled
    // far-future Helicon time. Each must survive a to_proto → from_proto round
    // trip byte-for-byte.
    #[test]
    fn round_trips_every_network_config() {
        for network_id in [1u32, 5u32, 12345u32] {
            let cfg = get_config(network_id);
            let pb = upgrades_to_proto(&cfg);
            let back = upgrades_from_proto(&pb).expect("upgrades_from_proto");
            assert_eq!(
                back, cfg,
                "network_upgrades round-trips for network_id={network_id}"
            );
        }
    }

    // The whole point of the conversion: the decoded schedule must reflect the
    // proto, NOT a reconstruction from network_id. A field that differs from the
    // network's canonical config proves the wire value won.
    #[test]
    fn decoded_config_reflects_proto_not_network_id() {
        let mut cfg = get_config(1);
        // A height a real mainnet config would never carry.
        cfg.apricot_phase_4_min_p_chain_height = 424_242;
        cfg.granite_epoch_duration = std::time::Duration::from_secs(7);
        let pb = upgrades_to_proto(&cfg);
        let back = upgrades_from_proto(&pb).expect("upgrades_from_proto");
        assert_eq!(
            back.apricot_phase_4_min_p_chain_height, 424_242,
            "the wire min-P-chain-height survives the round trip"
        );
        assert_eq!(
            back.granite_epoch_duration,
            std::time::Duration::from_secs(7),
            "the wire granite epoch duration survives the round trip"
        );
        assert_ne!(
            back,
            get_config(1),
            "the decoded config is the wire value, not get_config(network_id)"
        );
    }

    #[test]
    fn from_proto_rejects_nil_timestamp() {
        let cfg = get_config(1);
        let mut pb = upgrades_to_proto(&cfg);
        pb.banff_time = None;
        let err = upgrades_from_proto(&pb).expect_err("nil banff_time must be rejected");
        assert!(
            err.contains("banff_time"),
            "the error names the offending field, got: {err}"
        );
    }

    #[test]
    fn from_proto_rejects_wrong_length_stop_vertex_id() {
        let cfg = get_config(1);
        let mut pb = upgrades_to_proto(&cfg);
        pb.cortina_x_chain_stop_vertex_id = bytes::Bytes::from_static(&[0u8; 16]);
        let err =
            upgrades_from_proto(&pb).expect_err("a non-32-byte stop vertex id must be rejected");
        assert!(
            err.contains("cortina_x_chain_stop_vertex_id"),
            "the error names the offending field, got: {err}"
        );
    }

    // Helicon is unscheduled (9999-12-01 UTC) on every network — a far-future
    // instant that must still round-trip through the proto Timestamp range.
    #[test]
    fn unscheduled_helicon_time_round_trips() {
        let cfg = get_config(1);
        let pb = upgrades_to_proto(&cfg);
        let back = upgrades_from_proto(&pb).expect("upgrades_from_proto");
        assert_eq!(
            back.helicon_time, cfg.helicon_time,
            "the unscheduled far-future Helicon time round-trips"
        );
    }
}
