// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The resolved node [`Config`] (Go `config/node/config.go`; specs 12 §1.6).
//!
//! This is the shared contract read by `ava-node` and every subsystem:
//! `#[derive(Clone)]`, never serde-serialized on the hot path. Where the
//! owning crate already defines the Go-shaped sub-config it is embedded
//! directly (`ava_snow` snowball parameters via [`crate::subnets::Config`],
//! `ava_engine`'s [`AdaptiveTimeoutConfig`]/[`TargeterConfig`],
//! `ava_network::identity::Identity` for the staking TLS cert,
//! `ava_version::upgrade::UpgradeConfig`, `ava_genesis::params::*`,
//! `ava_logging`'s level/format). Where it does not (Go's `network.Config`,
//! `benchlist.Config`, `node.{HTTP,IP,Database,…}Config` have no Rust
//! equivalent yet), the holder struct is defined here — these are documented
//! seams to be re-homed when the owning crates grow config surfaces.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::SocketAddr;
use std::time::Duration;

use ava_engine::networking::timeout::AdaptiveTimeoutConfig;
use ava_engine::networking::tracker::TargeterConfig;
use ava_genesis::Bootstrapper;
use ava_genesis::params::{StakingConfig as GenesisStakingConfig, TxFeeConfig};
use ava_logging::{AvaLevel, Format};
use ava_network::identity::Identity;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use ava_version::upgrade::UpgradeConfig;

use crate::chain_config::ChainConfig;
use crate::subnets;

/// Go `logging.Config` (the node-level subset resolved from flags, 13 §15).
#[derive(Clone, Debug)]
pub struct LoggingConfig {
    /// Expanded `--log-dir`.
    pub directory: String,
    /// `--log-level`.
    pub log_level: AvaLevel,
    /// `--log-display-level` (inherits `--log-level` when unset).
    pub display_level: AvaLevel,
    /// `--log-format` (`auto` resolved against the tty at parse time).
    pub log_format: Format,
    /// `--log-disable-display-plugin-logs`.
    pub disable_writer_displaying: bool,
    /// `--log-rotater-max-size` (MiB).
    pub max_size: u32,
    /// `--log-rotater-max-files`.
    pub max_files: u32,
    /// `--log-rotater-max-age` (days).
    pub max_age: u32,
    /// `--log-rotater-compress-enabled`.
    pub compress: bool,
}

/// Go `node.APIConfig` + `node.APIIndexerConfig` (13 §6).
#[derive(Clone, Debug)]
pub struct ApiConfig {
    /// `--index-enabled`.
    pub index_api_enabled: bool,
    /// `--index-allow-incomplete`.
    pub index_allow_incomplete: bool,
    /// `--api-admin-enabled`.
    pub admin_api_enabled: bool,
    /// `--api-info-enabled`.
    pub info_api_enabled: bool,
    /// `--api-metrics-enabled`.
    pub metrics_api_enabled: bool,
    /// `--api-health-enabled`.
    pub health_api_enabled: bool,
}

/// Go `node.HTTPConfig` (+ embedded `server.HTTPConfig`, 13 §6).
#[derive(Clone, Debug)]
pub struct HttpConfig {
    /// `--http-read-timeout`.
    pub read_timeout: Duration,
    /// `--http-read-header-timeout`.
    pub read_header_timeout: Duration,
    /// `--http-write-timeout`.
    pub write_timeout: Duration,
    /// `--http-idle-timeout`.
    pub idle_timeout: Duration,
    /// The API enable/disable block.
    pub api_config: ApiConfig,
    /// `--http-host`.
    pub http_host: String,
    /// `--http-port`.
    pub http_port: u16,
    /// `--http-tls-enabled`.
    pub https_enabled: bool,
    /// The TLS key (from `--http-tls-key-file(-content)`), if provided.
    pub https_key: Vec<u8>,
    /// The TLS cert (from `--http-tls-cert-file(-content)`), if provided.
    pub https_cert: Vec<u8>,
    /// `--http-allowed-origins`.
    pub http_allowed_origins: Vec<String>,
    /// `--http-allowed-hosts`.
    pub http_allowed_hosts: Vec<String>,
    /// `--http-shutdown-timeout`.
    pub shutdown_timeout: Duration,
    /// `--http-shutdown-wait`.
    pub shutdown_wait: Duration,
}

/// Go `node.IPConfig` (13 §19).
#[derive(Clone, Debug)]
pub struct IpConfig {
    /// `--public-ip` (empty = resolve dynamically).
    pub public_ip: String,
    /// `--public-ip-resolution-service` (opendns/ifconfigco/ifconfigme).
    pub public_ip_resolution_service: String,
    /// `--public-ip-resolution-frequency`.
    pub public_ip_resolution_freq: Duration,
    /// `--staking-host`.
    pub listen_host: String,
    /// `--staking-port`.
    pub listen_port: u16,
}

/// Go `node.StakingSignerConfig` (13 §5).
#[derive(Clone, Debug)]
pub struct StakingSignerConfig {
    /// `--staking-ephemeral-signer-enabled`.
    pub ephemeral_signer_enabled: bool,
    /// `--staking-signer-key-file-content`.
    pub key_content: String,
    /// Expanded `--staking-signer-key-file` (only when no other option set).
    pub key_path: String,
    /// `--staking-rpc-signer-endpoint`.
    pub rpc_endpoint: String,
    /// Whether the key path was explicitly provided.
    pub key_path_is_set: bool,
}

/// Go `node.StakingConfig` (13 §5). The staking-economics block (`economics`)
/// comes from the flags on custom networks and from
/// `ava_genesis::params::get_staking_config` on Mainnet/Fuji.
#[derive(Clone, Debug)]
pub struct StakingConfig {
    /// `--sybil-protection-enabled`.
    pub sybil_protection_enabled: bool,
    /// `--sybil-protection-disabled-weight`.
    pub sybil_protection_disabled_weight: u64,
    /// `--partial-sync-primary-network`.
    pub partial_sync_primary_network: bool,
    /// Expanded `--staking-tls-key-file`.
    pub staking_tls_key_path: String,
    /// Expanded `--staking-tls-cert-file`.
    pub staking_tls_cert_path: String,
    /// The resolved staking TLS identity (Go `StakingTLSCert`): loaded,
    /// generated on disk, or ephemeral.
    pub identity: Identity,
    /// The BLS staking-signer selection.
    pub signer: StakingSignerConfig,
    /// Go `StakingConfig.StakingConfig` — the genesis staking-economics block.
    pub economics: GenesisStakingConfig,
}

/// Go `node.DatabaseConfig` (13 §20).
#[derive(Clone, Debug)]
pub struct DatabaseConfig {
    /// `--db-type` (leveldb/memdb/pebbledb; accepted verbatim for parity).
    pub name: String,
    /// `--db-read-only`.
    pub read_only: bool,
    /// `<expanded --db-dir>/<networkName>`.
    pub path: String,
    /// `--db-config-file(-content)` raw bytes, if provided.
    pub config: Vec<u8>,
}

/// Go `benchlist.Config` (13 §10). Defined here: `ava_engine`'s runtime
/// `BenchlistConfig` models the simplified consecutive-failure benchlist, not
/// the Go flag surface (documented seam).
#[derive(Clone, Debug)]
pub struct BenchlistConfig {
    /// `--benchlist-halflife`.
    pub halflife: Duration,
    /// `--benchlist-unbench-probability`.
    pub unbench_probability: f64,
    /// `--benchlist-bench-probability`.
    pub bench_probability: f64,
    /// `--benchlist-duration`.
    pub bench_duration: Duration,
    /// Computed `(1 - alphaConfidence/k) / 3` (never a flag).
    pub max_portion: f64,
}

/// Go `router.HealthConfig` (13 §11/§12).
#[derive(Clone, Debug)]
pub struct RouterHealthConfig {
    /// `--router-health-max-drop-rate`.
    pub max_drop_rate: f64,
    /// `--router-health-max-outstanding-requests`.
    pub max_outstanding_requests: u32,
    /// `--network-health-max-outstanding-request-duration`.
    pub max_outstanding_duration: Duration,
    /// `--network-maximum-timeout` (max run time of a request).
    pub max_run_time_requests: Duration,
    /// `--health-check-averager-halflife`.
    pub max_drop_rate_halflife: Duration,
}

/// Go `node.StateSyncConfig` (13 §13).
#[derive(Clone, Debug, Default)]
pub struct StateSyncConfig {
    /// `--state-sync-ips`.
    pub state_sync_ips: Vec<SocketAddr>,
    /// `--state-sync-ids`.
    pub state_sync_ids: Vec<NodeId>,
}

/// Go `node.BootstrapConfig` (13 §13).
#[derive(Clone, Debug)]
pub struct BootstrapConfig {
    /// The bootstrap beacons: from `--bootstrap-ips`/`--bootstrap-ids`, or
    /// sampled from the genesis list when both are unset.
    pub bootstrappers: Vec<Bootstrapper>,
    /// `--bootstrap-beacon-connection-timeout`.
    pub bootstrap_beacon_connection_timeout: Duration,
    /// `--bootstrap-max-time-get-ancestors`.
    pub bootstrap_max_time_get_ancestors: Duration,
    /// `--bootstrap-ancestors-max-containers-sent`.
    pub bootstrap_ancestors_max_containers_sent: u32,
    /// `--bootstrap-ancestors-max-containers-received`.
    pub bootstrap_ancestors_max_containers_received: u32,
}

/// Go `profiler.Config` (13 §17).
#[derive(Clone, Debug)]
pub struct ProfilerConfig {
    /// Expanded `--profile-dir`.
    pub dir: String,
    /// `--profile-continuous-enabled`.
    pub enabled: bool,
    /// `--profile-continuous-freq`.
    pub freq: Duration,
    /// `--profile-continuous-max-files`.
    pub max_num_files: i64,
}

/// Go `trace.ExporterType` (13 §22).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TraceExporterType {
    /// Tracing disabled.
    Disabled,
    /// OTLP over gRPC.
    Grpc,
    /// OTLP over HTTP.
    Http,
}

/// Go `trace.Config` (13 §22).
#[derive(Clone, Debug)]
pub struct TraceConfig {
    /// `--tracing-exporter-type`.
    pub exporter_type: TraceExporterType,
    /// `--tracing-endpoint`.
    pub endpoint: String,
    /// `--tracing-insecure`.
    pub insecure: bool,
    /// `--tracing-headers`.
    pub headers: HashMap<String, String>,
    /// `--tracing-sample-rate`.
    pub trace_sample_rate: f64,
    /// `constants.AppName`.
    pub app_name: String,
    /// `version.Current.String()`.
    pub version: String,
}

/// Go `network.Config` (specs 05 owns the runtime; the flag-resolved shape
/// lives here as a documented seam — `ava_network` exposes only the runtime
/// `PeerConfig`). Field order/grouping follows `network/config.go` (13 §8/§9).
#[derive(Clone, Debug)]
pub struct NetworkConfig {
    // --- inbound connection throttling ---
    /// `--network-inbound-connection-throttling-max-conns-per-sec`.
    pub max_inbound_conns_per_sec: f64,
    /// `--network-inbound-connection-throttling-cooldown`.
    pub inbound_connection_upgrade_cooldown: Duration,
    /// `ceil(maxInboundConnsPerSec * cooldown.Seconds())` (computed).
    pub max_recent_conns_upgraded: u64,

    // --- inbound message throttling ---
    /// `--throttler-inbound-at-large-alloc-size`.
    pub inbound_throttler_at_large_alloc_size: u64,
    /// `--throttler-inbound-validator-alloc-size`.
    pub inbound_throttler_vdr_alloc_size: u64,
    /// `--throttler-inbound-node-max-at-large-bytes`.
    pub inbound_throttler_node_max_at_large_bytes: u64,
    /// `--throttler-inbound-bandwidth-refill-rate`.
    pub inbound_throttler_bandwidth_refill_rate: u64,
    /// `--throttler-inbound-bandwidth-max-burst-size`.
    pub inbound_throttler_bandwidth_max_burst_size: u64,
    /// `--throttler-inbound-node-max-processing-msgs`.
    pub inbound_throttler_max_processing_msgs_per_node: u64,
    /// `--throttler-inbound-cpu-max-recheck-delay`.
    pub inbound_throttler_cpu_max_recheck_delay: Duration,
    /// `--throttler-inbound-disk-max-recheck-delay`.
    pub inbound_throttler_disk_max_recheck_delay: Duration,

    // --- outbound message throttling ---
    /// `--throttler-outbound-at-large-alloc-size`.
    pub outbound_throttler_at_large_alloc_size: u64,
    /// `--throttler-outbound-validator-alloc-size`.
    pub outbound_throttler_vdr_alloc_size: u64,
    /// `--throttler-outbound-node-max-at-large-bytes`.
    pub outbound_throttler_node_max_at_large_bytes: u64,

    // --- network health ---
    /// Health checks enabled iff sybil protection is enabled.
    pub health_enabled: bool,
    /// `--network-health-max-time-since-msg-sent`.
    pub health_max_time_since_msg_sent: Duration,
    /// `--network-health-max-time-since-msg-received`.
    pub health_max_time_since_msg_received: Duration,
    /// `--network-health-max-portion-send-queue-full`.
    pub health_max_portion_send_queue_bytes_full: f64,
    /// `--network-health-min-conn-peers`.
    pub health_min_connected_peers: u64,
    /// `--network-health-max-send-fail-rate`.
    pub health_max_send_fail_rate: f64,
    /// `--health-check-averager-halflife` (shared).
    pub health_send_fail_rate_halflife: Duration,
    /// `--network-no-ingress-connections-grace-period`.
    pub no_ingress_validator_connection_grace_period: Duration,

    // --- proxy / dialer / TLS ---
    /// `--network-tcp-proxy-enabled`.
    pub proxy_enabled: bool,
    /// `--network-tcp-proxy-read-timeout`.
    pub proxy_read_header_timeout: Duration,
    /// `--network-outbound-connection-throttling-rps`.
    pub dialer_throttle_rps: u32,
    /// `--network-outbound-connection-timeout`.
    pub dialer_connection_timeout: Duration,
    /// `--network-tls-key-log-file-unsafe`.
    pub tls_key_log_file: String,

    // --- timeouts / gossip / delays ---
    /// `--network-ping-timeout`.
    pub ping_pong_timeout: Duration,
    /// `--network-read-handshake-timeout`.
    pub read_handshake_timeout: Duration,
    /// `--network-peer-list-num-validator-ips`.
    pub peer_list_num_validator_ips: u32,
    /// `--network-peer-list-pull-gossip-frequency`.
    pub peer_list_pull_gossip_freq: Duration,
    /// `--network-peer-list-bloom-reset-frequency`.
    pub peer_list_bloom_reset_freq: Duration,
    /// `--network-max-reconnect-delay`.
    pub max_reconnect_delay: Duration,
    /// `--network-initial-reconnect-delay`.
    pub initial_reconnect_delay: Duration,

    // --- misc ---
    /// `--network-max-clock-difference`.
    pub max_clock_difference: Duration,
    /// `--network-compression-type` (zstd/none).
    pub compression_type: String,
    /// `--network-ping-frequency`.
    pub ping_frequency: Duration,
    /// `--network-allow-private-ips` (NETWORK-DEPENDENT default, 13 §8).
    pub allow_private_ips: bool,
    /// `--uptime-metric-freq`.
    pub uptime_metric_freq: Duration,
    /// `--network-maximum-inbound-timeout`.
    pub maximum_inbound_message_timeout: Duration,
    /// `--acp-support` ∪ scheduled − activated (13 §3).
    pub supported_acps: BTreeSet<u32>,
    /// `--acp-object` − activated.
    pub objected_acps: BTreeSet<u32>,
    /// `--network-require-validator-to-connect`.
    pub require_validator_to_connect: bool,
    /// `--network-peer-read-buffer-size`.
    pub peer_read_buffer_size: u64,
    /// `--network-peer-write-buffer-size`.
    pub peer_write_buffer_size: u64,
}

/// Go `node.Config` — everything the node needs, resolved once at startup
/// (`config/node/config.go`; field order follows `GetNodeConfig`).
#[derive(Clone, Debug)]
pub struct Config {
    /// Expanded `--plugin-dir` (created when defaulted, asserted when set).
    pub plugin_dir: String,
    /// `--consensus-shutdown-timeout`.
    pub consensus_shutdown_timeout: Duration,
    /// `--consensus-frontier-poll-frequency`.
    pub frontier_poll_frequency: Duration,
    /// `--consensus-app-concurrency` (> 0).
    pub consensus_app_concurrency: u32,
    /// `--proposervm-use-current-height`.
    pub use_current_height: bool,
    /// The logging block (13 §15).
    pub logging_config: LoggingConfig,
    /// The parsed `--network-id` (1/5/12345/custom).
    pub network_id: u32,
    /// The database block (13 §20).
    pub database_config: DatabaseConfig,
    /// The public-IP / listen block (13 §19).
    pub ip_config: IpConfig,
    /// The staking block (13 §5).
    pub staking_config: StakingConfig,
    /// `--track-subnets` (never contains the Primary Network).
    pub tracked_subnets: BTreeSet<Id>,
    /// The HTTP/API block (13 §6).
    pub http_config: HttpConfig,
    /// `--health-check-frequency`.
    pub health_check_freq: Duration,
    /// The router health block (13 §11).
    pub router_health_config: RouterHealthConfig,
    /// `--meter-vms-enabled`.
    pub meter_vm_enabled: bool,
    /// The adaptive timeout block (owned by `ava-engine`, 13 §8).
    pub adaptive_timeout_config: AdaptiveTimeoutConfig,
    /// The network-wide upgrade schedule (embedded per network; a custom
    /// `--upgrade-file(-content)` is carried raw in
    /// [`Config::custom_upgrade_bytes`]).
    pub upgrade_config: UpgradeConfig,
    /// Raw custom upgrade JSON for custom networks (typed parsing is the
    /// `ava-version` seam: `UpgradeConfig` has no serde yet).
    pub custom_upgrade_bytes: Option<Vec<u8>>,
    /// The networking block (13 §8/§9).
    pub network_config: NetworkConfig,
    /// Per-subnet configs (always contains the Primary Network entry).
    pub subnet_configs: HashMap<Id, subnets::Config>,
    /// `--proposervm-min-block-delay`.
    pub proposer_min_block_delay: Duration,
    /// The benchlist block (13 §10).
    pub benchlist_config: BenchlistConfig,
    /// `--fd-limit`.
    pub fd_limit: u64,
    /// The tx-fee block: flags on custom networks, genesis params on
    /// Mainnet/Fuji (13 §4).
    pub tx_fee_config: TxFeeConfig,
    /// The P-Chain genesis bytes (embedded or custom; 13 §21).
    pub genesis_bytes: Vec<u8>,
    /// The AVAX asset ID derived from the genesis.
    pub avax_asset_id: Id,
    /// The state-sync peer lists (13 §13).
    pub state_sync_config: StateSyncConfig,
    /// The bootstrap block (13 §13).
    pub bootstrap_config: BootstrapConfig,
    /// Per-chain config/upgrade blobs keyed by alias (13 §14).
    pub chain_configs: HashMap<String, ChainConfig>,
    /// The profiler block (13 §17).
    pub profiler_config: ProfilerConfig,
    /// `--vm-aliases-file(-content)`.
    pub vm_aliases: HashMap<Id, Vec<String>>,
    /// `--chain-aliases-file(-content)`.
    pub chain_aliases: HashMap<Id, Vec<String>>,
    /// `--system-tracker-frequency`.
    pub system_tracker_frequency: Duration,
    /// `--system-tracker-processing-halflife`.
    pub system_tracker_processing_halflife: Duration,
    /// `--system-tracker-cpu-halflife`.
    pub system_tracker_cpu_halflife: Duration,
    /// `--system-tracker-disk-halflife`.
    pub system_tracker_disk_halflife: Duration,
    /// `--system-tracker-disk-required-available-space-percentage`.
    pub required_available_disk_space_percentage: u64,
    /// `--system-tracker-disk-warning-available-space-percentage`.
    pub warning_available_disk_space_percentage: u64,
    /// The CPU targeter block (owned by `ava-engine`, 13 §9).
    pub cpu_targeter_config: TargeterConfig,
    /// The disk targeter block (owned by `ava-engine`, 13 §9).
    pub disk_targeter_config: TargeterConfig,
    /// The tracing block (13 §22).
    pub trace_config: TraceConfig,
    /// Expanded `--chain-data-dir`.
    pub chain_data_dir: String,
    /// Expanded `--process-context-file`.
    pub process_context_file_path: String,
    /// The keys explicitly provided at any non-default layer (Go
    /// `ProvidedFlags` keeps key→value; the key set is what the admin API and
    /// metrics need — 13 §23).
    pub provided_flags: BTreeMap<String, String>,
}
