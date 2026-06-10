// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The verbatim flag-key constants (Go `config/keys.go`, 1:1).
//!
//! 206 keys total (specs 13 §24): the 205-key `const` block plus
//! `http-write-timeout` (declared separately in Go with a `#nosec G101`
//! comment -- purely cosmetic). Constants are listed sorted by flag name; the
//! golden `flag_parity` snapshot (tests/vectors/config/flags.json) is the
//! drift guard against the live Go tree.

/// `--acp-object`.
pub const KEY_ACP_OBJECT: &str = "acp-object";
/// `--acp-support`.
pub const KEY_ACP_SUPPORT: &str = "acp-support";
/// `--api-admin-enabled`.
pub const KEY_API_ADMIN_ENABLED: &str = "api-admin-enabled";
/// `--api-health-enabled`.
pub const KEY_API_HEALTH_ENABLED: &str = "api-health-enabled";
/// `--api-info-enabled`.
pub const KEY_API_INFO_ENABLED: &str = "api-info-enabled";
/// `--api-metrics-enabled`.
pub const KEY_API_METRICS_ENABLED: &str = "api-metrics-enabled";
/// `--benchlist-bench-probability`.
pub const KEY_BENCHLIST_BENCH_PROBABILITY: &str = "benchlist-bench-probability";
/// `--benchlist-duration`.
pub const KEY_BENCHLIST_DURATION: &str = "benchlist-duration";
/// `--benchlist-halflife`.
pub const KEY_BENCHLIST_HALFLIFE: &str = "benchlist-halflife";
/// `--benchlist-unbench-probability`.
pub const KEY_BENCHLIST_UNBENCH_PROBABILITY: &str = "benchlist-unbench-probability";
/// `--bootstrap-ancestors-max-containers-received`.
pub const KEY_BOOTSTRAP_ANCESTORS_MAX_CONTAINERS_RECEIVED: &str = "bootstrap-ancestors-max-containers-received";
/// `--bootstrap-ancestors-max-containers-sent`.
pub const KEY_BOOTSTRAP_ANCESTORS_MAX_CONTAINERS_SENT: &str = "bootstrap-ancestors-max-containers-sent";
/// `--bootstrap-beacon-connection-timeout`.
pub const KEY_BOOTSTRAP_BEACON_CONNECTION_TIMEOUT: &str = "bootstrap-beacon-connection-timeout";
/// `--bootstrap-ids`.
pub const KEY_BOOTSTRAP_IDS: &str = "bootstrap-ids";
/// `--bootstrap-ips`.
pub const KEY_BOOTSTRAP_IPS: &str = "bootstrap-ips";
/// `--bootstrap-max-time-get-ancestors`.
pub const KEY_BOOTSTRAP_MAX_TIME_GET_ANCESTORS: &str = "bootstrap-max-time-get-ancestors";
/// `--chain-aliases-file`.
pub const KEY_CHAIN_ALIASES_FILE: &str = "chain-aliases-file";
/// `--chain-aliases-file-content`.
pub const KEY_CHAIN_ALIASES_FILE_CONTENT: &str = "chain-aliases-file-content";
/// `--chain-config-content`.
pub const KEY_CHAIN_CONFIG_CONTENT: &str = "chain-config-content";
/// `--chain-config-dir`.
pub const KEY_CHAIN_CONFIG_DIR: &str = "chain-config-dir";
/// `--chain-data-dir`.
pub const KEY_CHAIN_DATA_DIR: &str = "chain-data-dir";
/// `--config-file`.
pub const KEY_CONFIG_FILE: &str = "config-file";
/// `--config-file-content`.
pub const KEY_CONFIG_FILE_CONTENT: &str = "config-file-content";
/// `--config-file-content-type`.
pub const KEY_CONFIG_FILE_CONTENT_TYPE: &str = "config-file-content-type";
/// `--consensus-app-concurrency`.
pub const KEY_CONSENSUS_APP_CONCURRENCY: &str = "consensus-app-concurrency";
/// `--consensus-frontier-poll-frequency`.
pub const KEY_CONSENSUS_FRONTIER_POLL_FREQUENCY: &str = "consensus-frontier-poll-frequency";
/// `--consensus-shutdown-timeout`.
pub const KEY_CONSENSUS_SHUTDOWN_TIMEOUT: &str = "consensus-shutdown-timeout";
/// `--create-asset-tx-fee`.
pub const KEY_CREATE_ASSET_TX_FEE: &str = "create-asset-tx-fee";
/// `--data-dir`.
pub const KEY_DATA_DIR: &str = "data-dir";
/// `--db-config-file`.
pub const KEY_DB_CONFIG_FILE: &str = "db-config-file";
/// `--db-config-file-content`.
pub const KEY_DB_CONFIG_FILE_CONTENT: &str = "db-config-file-content";
/// `--db-dir`.
pub const KEY_DB_DIR: &str = "db-dir";
/// `--db-read-only`.
pub const KEY_DB_READ_ONLY: &str = "db-read-only";
/// `--db-type`.
pub const KEY_DB_TYPE: &str = "db-type";
/// `--dynamic-fees-bandwidth-weight`.
pub const KEY_DYNAMIC_FEES_BANDWIDTH_WEIGHT: &str = "dynamic-fees-bandwidth-weight";
/// `--dynamic-fees-compute-weight`.
pub const KEY_DYNAMIC_FEES_COMPUTE_WEIGHT: &str = "dynamic-fees-compute-weight";
/// `--dynamic-fees-db-read-weight`.
pub const KEY_DYNAMIC_FEES_DB_READ_WEIGHT: &str = "dynamic-fees-db-read-weight";
/// `--dynamic-fees-db-write-weight`.
pub const KEY_DYNAMIC_FEES_DB_WRITE_WEIGHT: &str = "dynamic-fees-db-write-weight";
/// `--dynamic-fees-excess-conversion-constant`.
pub const KEY_DYNAMIC_FEES_EXCESS_CONVERSION_CONSTANT: &str = "dynamic-fees-excess-conversion-constant";
/// `--dynamic-fees-max-gas-capacity`.
pub const KEY_DYNAMIC_FEES_MAX_GAS_CAPACITY: &str = "dynamic-fees-max-gas-capacity";
/// `--dynamic-fees-max-gas-per-second`.
pub const KEY_DYNAMIC_FEES_MAX_GAS_PER_SECOND: &str = "dynamic-fees-max-gas-per-second";
/// `--dynamic-fees-min-gas-price`.
pub const KEY_DYNAMIC_FEES_MIN_GAS_PRICE: &str = "dynamic-fees-min-gas-price";
/// `--dynamic-fees-target-gas-per-second`.
pub const KEY_DYNAMIC_FEES_TARGET_GAS_PER_SECOND: &str = "dynamic-fees-target-gas-per-second";
/// `--fd-limit`.
pub const KEY_FD_LIMIT: &str = "fd-limit";
/// `--genesis-file`.
pub const KEY_GENESIS_FILE: &str = "genesis-file";
/// `--genesis-file-content`.
pub const KEY_GENESIS_FILE_CONTENT: &str = "genesis-file-content";
/// `--health-check-averager-halflife`.
pub const KEY_HEALTH_CHECK_AVERAGER_HALFLIFE: &str = "health-check-averager-halflife";
/// `--health-check-frequency`.
pub const KEY_HEALTH_CHECK_FREQUENCY: &str = "health-check-frequency";
/// `--http-allowed-hosts`.
pub const KEY_HTTP_ALLOWED_HOSTS: &str = "http-allowed-hosts";
/// `--http-allowed-origins`.
pub const KEY_HTTP_ALLOWED_ORIGINS: &str = "http-allowed-origins";
/// `--http-host`.
pub const KEY_HTTP_HOST: &str = "http-host";
/// `--http-idle-timeout`.
pub const KEY_HTTP_IDLE_TIMEOUT: &str = "http-idle-timeout";
/// `--http-port`.
pub const KEY_HTTP_PORT: &str = "http-port";
/// `--http-read-header-timeout`.
pub const KEY_HTTP_READ_HEADER_TIMEOUT: &str = "http-read-header-timeout";
/// `--http-read-timeout`.
pub const KEY_HTTP_READ_TIMEOUT: &str = "http-read-timeout";
/// `--http-shutdown-timeout`.
pub const KEY_HTTP_SHUTDOWN_TIMEOUT: &str = "http-shutdown-timeout";
/// `--http-shutdown-wait`.
pub const KEY_HTTP_SHUTDOWN_WAIT: &str = "http-shutdown-wait";
/// `--http-tls-cert-file`.
pub const KEY_HTTP_TLS_CERT_FILE: &str = "http-tls-cert-file";
/// `--http-tls-cert-file-content`.
pub const KEY_HTTP_TLS_CERT_FILE_CONTENT: &str = "http-tls-cert-file-content";
/// `--http-tls-enabled`.
pub const KEY_HTTP_TLS_ENABLED: &str = "http-tls-enabled";
/// `--http-tls-key-file`.
pub const KEY_HTTP_TLS_KEY_FILE: &str = "http-tls-key-file";
/// `--http-tls-key-file-content`.
pub const KEY_HTTP_TLS_KEY_FILE_CONTENT: &str = "http-tls-key-file-content";
/// `--http-write-timeout`.
pub const KEY_HTTP_WRITE_TIMEOUT: &str = "http-write-timeout";
/// `--index-allow-incomplete`.
pub const KEY_INDEX_ALLOW_INCOMPLETE: &str = "index-allow-incomplete";
/// `--index-enabled`.
pub const KEY_INDEX_ENABLED: &str = "index-enabled";
/// `--log-dir`.
pub const KEY_LOG_DIR: &str = "log-dir";
/// `--log-disable-display-plugin-logs`.
pub const KEY_LOG_DISABLE_DISPLAY_PLUGIN_LOGS: &str = "log-disable-display-plugin-logs";
/// `--log-display-level`.
pub const KEY_LOG_DISPLAY_LEVEL: &str = "log-display-level";
/// `--log-format`.
pub const KEY_LOG_FORMAT: &str = "log-format";
/// `--log-level`.
pub const KEY_LOG_LEVEL: &str = "log-level";
/// `--log-rotater-compress-enabled`.
pub const KEY_LOG_ROTATER_COMPRESS_ENABLED: &str = "log-rotater-compress-enabled";
/// `--log-rotater-max-age`.
pub const KEY_LOG_ROTATER_MAX_AGE: &str = "log-rotater-max-age";
/// `--log-rotater-max-files`.
pub const KEY_LOG_ROTATER_MAX_FILES: &str = "log-rotater-max-files";
/// `--log-rotater-max-size`.
pub const KEY_LOG_ROTATER_MAX_SIZE: &str = "log-rotater-max-size";
/// `--max-stake-duration`.
pub const KEY_MAX_STAKE_DURATION: &str = "max-stake-duration";
/// `--max-validator-stake`.
pub const KEY_MAX_VALIDATOR_STAKE: &str = "max-validator-stake";
/// `--meter-vms-enabled`.
pub const KEY_METER_VMS_ENABLED: &str = "meter-vms-enabled";
/// `--min-delegation-fee`.
pub const KEY_MIN_DELEGATION_FEE: &str = "min-delegation-fee";
/// `--min-delegator-stake`.
pub const KEY_MIN_DELEGATOR_STAKE: &str = "min-delegator-stake";
/// `--min-stake-duration`.
pub const KEY_MIN_STAKE_DURATION: &str = "min-stake-duration";
/// `--min-validator-stake`.
pub const KEY_MIN_VALIDATOR_STAKE: &str = "min-validator-stake";
/// `--network-allow-private-ips`.
pub const KEY_NETWORK_ALLOW_PRIVATE_IPS: &str = "network-allow-private-ips";
/// `--network-compression-type`.
pub const KEY_NETWORK_COMPRESSION_TYPE: &str = "network-compression-type";
/// `--network-health-max-outstanding-request-duration`.
pub const KEY_NETWORK_HEALTH_MAX_OUTSTANDING_REQUEST_DURATION: &str = "network-health-max-outstanding-request-duration";
/// `--network-health-max-portion-send-queue-full`.
pub const KEY_NETWORK_HEALTH_MAX_PORTION_SEND_QUEUE_FULL: &str = "network-health-max-portion-send-queue-full";
/// `--network-health-max-send-fail-rate`.
pub const KEY_NETWORK_HEALTH_MAX_SEND_FAIL_RATE: &str = "network-health-max-send-fail-rate";
/// `--network-health-max-time-since-msg-received`.
pub const KEY_NETWORK_HEALTH_MAX_TIME_SINCE_MSG_RECEIVED: &str = "network-health-max-time-since-msg-received";
/// `--network-health-max-time-since-msg-sent`.
pub const KEY_NETWORK_HEALTH_MAX_TIME_SINCE_MSG_SENT: &str = "network-health-max-time-since-msg-sent";
/// `--network-health-min-conn-peers`.
pub const KEY_NETWORK_HEALTH_MIN_CONN_PEERS: &str = "network-health-min-conn-peers";
/// `--network-id`.
pub const KEY_NETWORK_ID: &str = "network-id";
/// `--network-inbound-connection-throttling-cooldown`.
pub const KEY_NETWORK_INBOUND_CONNECTION_THROTTLING_COOLDOWN: &str = "network-inbound-connection-throttling-cooldown";
/// `--network-inbound-connection-throttling-max-conns-per-sec`.
pub const KEY_NETWORK_INBOUND_CONNECTION_THROTTLING_MAX_CONNS_PER_SEC: &str = "network-inbound-connection-throttling-max-conns-per-sec";
/// `--network-initial-reconnect-delay`.
pub const KEY_NETWORK_INITIAL_RECONNECT_DELAY: &str = "network-initial-reconnect-delay";
/// `--network-initial-timeout`.
pub const KEY_NETWORK_INITIAL_TIMEOUT: &str = "network-initial-timeout";
/// `--network-max-clock-difference`.
pub const KEY_NETWORK_MAX_CLOCK_DIFFERENCE: &str = "network-max-clock-difference";
/// `--network-max-reconnect-delay`.
pub const KEY_NETWORK_MAX_RECONNECT_DELAY: &str = "network-max-reconnect-delay";
/// `--network-maximum-inbound-timeout`.
pub const KEY_NETWORK_MAXIMUM_INBOUND_TIMEOUT: &str = "network-maximum-inbound-timeout";
/// `--network-maximum-timeout`.
pub const KEY_NETWORK_MAXIMUM_TIMEOUT: &str = "network-maximum-timeout";
/// `--network-minimum-timeout`.
pub const KEY_NETWORK_MINIMUM_TIMEOUT: &str = "network-minimum-timeout";
/// `--network-no-ingress-connections-grace-period`.
pub const KEY_NETWORK_NO_INGRESS_CONNECTIONS_GRACE_PERIOD: &str = "network-no-ingress-connections-grace-period";
/// `--network-outbound-connection-throttling-rps`.
pub const KEY_NETWORK_OUTBOUND_CONNECTION_THROTTLING_RPS: &str = "network-outbound-connection-throttling-rps";
/// `--network-outbound-connection-timeout`.
pub const KEY_NETWORK_OUTBOUND_CONNECTION_TIMEOUT: &str = "network-outbound-connection-timeout";
/// `--network-peer-list-bloom-reset-frequency`.
pub const KEY_NETWORK_PEER_LIST_BLOOM_RESET_FREQUENCY: &str = "network-peer-list-bloom-reset-frequency";
/// `--network-peer-list-num-validator-ips`.
pub const KEY_NETWORK_PEER_LIST_NUM_VALIDATOR_IPS: &str = "network-peer-list-num-validator-ips";
/// `--network-peer-list-pull-gossip-frequency`.
pub const KEY_NETWORK_PEER_LIST_PULL_GOSSIP_FREQUENCY: &str = "network-peer-list-pull-gossip-frequency";
/// `--network-peer-read-buffer-size`.
pub const KEY_NETWORK_PEER_READ_BUFFER_SIZE: &str = "network-peer-read-buffer-size";
/// `--network-peer-write-buffer-size`.
pub const KEY_NETWORK_PEER_WRITE_BUFFER_SIZE: &str = "network-peer-write-buffer-size";
/// `--network-ping-frequency`.
pub const KEY_NETWORK_PING_FREQUENCY: &str = "network-ping-frequency";
/// `--network-ping-timeout`.
pub const KEY_NETWORK_PING_TIMEOUT: &str = "network-ping-timeout";
/// `--network-read-handshake-timeout`.
pub const KEY_NETWORK_READ_HANDSHAKE_TIMEOUT: &str = "network-read-handshake-timeout";
/// `--network-require-validator-to-connect`.
pub const KEY_NETWORK_REQUIRE_VALIDATOR_TO_CONNECT: &str = "network-require-validator-to-connect";
/// `--network-tcp-proxy-enabled`.
pub const KEY_NETWORK_TCP_PROXY_ENABLED: &str = "network-tcp-proxy-enabled";
/// `--network-tcp-proxy-read-timeout`.
pub const KEY_NETWORK_TCP_PROXY_READ_TIMEOUT: &str = "network-tcp-proxy-read-timeout";
/// `--network-timeout-coefficient`.
pub const KEY_NETWORK_TIMEOUT_COEFFICIENT: &str = "network-timeout-coefficient";
/// `--network-timeout-halflife`.
pub const KEY_NETWORK_TIMEOUT_HALFLIFE: &str = "network-timeout-halflife";
/// `--network-tls-key-log-file-unsafe`.
pub const KEY_NETWORK_TLS_KEY_LOG_FILE_UNSAFE: &str = "network-tls-key-log-file-unsafe";
/// `--partial-sync-primary-network`.
pub const KEY_PARTIAL_SYNC_PRIMARY_NETWORK: &str = "partial-sync-primary-network";
/// `--plugin-dir`.
pub const KEY_PLUGIN_DIR: &str = "plugin-dir";
/// `--process-context-file`.
pub const KEY_PROCESS_CONTEXT_FILE: &str = "process-context-file";
/// `--profile-continuous-enabled`.
pub const KEY_PROFILE_CONTINUOUS_ENABLED: &str = "profile-continuous-enabled";
/// `--profile-continuous-freq`.
pub const KEY_PROFILE_CONTINUOUS_FREQ: &str = "profile-continuous-freq";
/// `--profile-continuous-max-files`.
pub const KEY_PROFILE_CONTINUOUS_MAX_FILES: &str = "profile-continuous-max-files";
/// `--profile-dir`.
pub const KEY_PROFILE_DIR: &str = "profile-dir";
/// `--proposervm-min-block-delay`.
pub const KEY_PROPOSERVM_MIN_BLOCK_DELAY: &str = "proposervm-min-block-delay";
/// `--proposervm-use-current-height`.
pub const KEY_PROPOSERVM_USE_CURRENT_HEIGHT: &str = "proposervm-use-current-height";
/// `--public-ip`.
pub const KEY_PUBLIC_IP: &str = "public-ip";
/// `--public-ip-resolution-frequency`.
pub const KEY_PUBLIC_IP_RESOLUTION_FREQUENCY: &str = "public-ip-resolution-frequency";
/// `--public-ip-resolution-service`.
pub const KEY_PUBLIC_IP_RESOLUTION_SERVICE: &str = "public-ip-resolution-service";
/// `--router-health-max-drop-rate`.
pub const KEY_ROUTER_HEALTH_MAX_DROP_RATE: &str = "router-health-max-drop-rate";
/// `--router-health-max-outstanding-requests`.
pub const KEY_ROUTER_HEALTH_MAX_OUTSTANDING_REQUESTS: &str = "router-health-max-outstanding-requests";
/// `--simplex-max-network-delay`.
pub const KEY_SIMPLEX_MAX_NETWORK_DELAY: &str = "simplex-max-network-delay";
/// `--simplex-max-rebroadcast-wait`.
pub const KEY_SIMPLEX_MAX_REBROADCAST_WAIT: &str = "simplex-max-rebroadcast-wait";
/// `--snow-commit-threshold`.
pub const KEY_SNOW_COMMIT_THRESHOLD: &str = "snow-commit-threshold";
/// `--snow-concurrent-repolls`.
pub const KEY_SNOW_CONCURRENT_REPOLLS: &str = "snow-concurrent-repolls";
/// `--snow-confidence-quorum-size`.
pub const KEY_SNOW_CONFIDENCE_QUORUM_SIZE: &str = "snow-confidence-quorum-size";
/// `--snow-max-processing`.
pub const KEY_SNOW_MAX_PROCESSING: &str = "snow-max-processing";
/// `--snow-max-time-processing`.
pub const KEY_SNOW_MAX_TIME_PROCESSING: &str = "snow-max-time-processing";
/// `--snow-optimal-processing`.
pub const KEY_SNOW_OPTIMAL_PROCESSING: &str = "snow-optimal-processing";
/// `--snow-preference-quorum-size`.
pub const KEY_SNOW_PREFERENCE_QUORUM_SIZE: &str = "snow-preference-quorum-size";
/// `--snow-quorum-size`.
pub const KEY_SNOW_QUORUM_SIZE: &str = "snow-quorum-size";
/// `--snow-sample-size`.
pub const KEY_SNOW_SAMPLE_SIZE: &str = "snow-sample-size";
/// `--stake-max-consumption-rate`.
pub const KEY_STAKE_MAX_CONSUMPTION_RATE: &str = "stake-max-consumption-rate";
/// `--stake-min-consumption-rate`.
pub const KEY_STAKE_MIN_CONSUMPTION_RATE: &str = "stake-min-consumption-rate";
/// `--stake-minting-period`.
pub const KEY_STAKE_MINTING_PERIOD: &str = "stake-minting-period";
/// `--stake-supply-cap`.
pub const KEY_STAKE_SUPPLY_CAP: &str = "stake-supply-cap";
/// `--staking-ephemeral-cert-enabled`.
pub const KEY_STAKING_EPHEMERAL_CERT_ENABLED: &str = "staking-ephemeral-cert-enabled";
/// `--staking-ephemeral-signer-enabled`.
pub const KEY_STAKING_EPHEMERAL_SIGNER_ENABLED: &str = "staking-ephemeral-signer-enabled";
/// `--staking-host`.
pub const KEY_STAKING_HOST: &str = "staking-host";
/// `--staking-port`.
pub const KEY_STAKING_PORT: &str = "staking-port";
/// `--staking-rpc-signer-endpoint`.
pub const KEY_STAKING_RPC_SIGNER_ENDPOINT: &str = "staking-rpc-signer-endpoint";
/// `--staking-signer-key-file`.
pub const KEY_STAKING_SIGNER_KEY_FILE: &str = "staking-signer-key-file";
/// `--staking-signer-key-file-content`.
pub const KEY_STAKING_SIGNER_KEY_FILE_CONTENT: &str = "staking-signer-key-file-content";
/// `--staking-tls-cert-file`.
pub const KEY_STAKING_TLS_CERT_FILE: &str = "staking-tls-cert-file";
/// `--staking-tls-cert-file-content`.
pub const KEY_STAKING_TLS_CERT_FILE_CONTENT: &str = "staking-tls-cert-file-content";
/// `--staking-tls-key-file`.
pub const KEY_STAKING_TLS_KEY_FILE: &str = "staking-tls-key-file";
/// `--staking-tls-key-file-content`.
pub const KEY_STAKING_TLS_KEY_FILE_CONTENT: &str = "staking-tls-key-file-content";
/// `--state-sync-ids`.
pub const KEY_STATE_SYNC_IDS: &str = "state-sync-ids";
/// `--state-sync-ips`.
pub const KEY_STATE_SYNC_IPS: &str = "state-sync-ips";
/// `--subnet-config-content`.
pub const KEY_SUBNET_CONFIG_CONTENT: &str = "subnet-config-content";
/// `--subnet-config-dir`.
pub const KEY_SUBNET_CONFIG_DIR: &str = "subnet-config-dir";
/// `--sybil-protection-disabled-weight`.
pub const KEY_SYBIL_PROTECTION_DISABLED_WEIGHT: &str = "sybil-protection-disabled-weight";
/// `--sybil-protection-enabled`.
pub const KEY_SYBIL_PROTECTION_ENABLED: &str = "sybil-protection-enabled";
/// `--system-tracker-cpu-halflife`.
pub const KEY_SYSTEM_TRACKER_CPU_HALFLIFE: &str = "system-tracker-cpu-halflife";
/// `--system-tracker-disk-halflife`.
pub const KEY_SYSTEM_TRACKER_DISK_HALFLIFE: &str = "system-tracker-disk-halflife";
/// `--system-tracker-disk-required-available-space`.
pub const KEY_SYSTEM_TRACKER_DISK_REQUIRED_AVAILABLE_SPACE: &str = "system-tracker-disk-required-available-space";
/// `--system-tracker-disk-required-available-space-percentage`.
pub const KEY_SYSTEM_TRACKER_DISK_REQUIRED_AVAILABLE_SPACE_PERCENTAGE: &str = "system-tracker-disk-required-available-space-percentage";
/// `--system-tracker-disk-warning-available-space-percentage`.
pub const KEY_SYSTEM_TRACKER_DISK_WARNING_AVAILABLE_SPACE_PERCENTAGE: &str = "system-tracker-disk-warning-available-space-percentage";
/// `--system-tracker-disk-warning-threshold-available-space`.
pub const KEY_SYSTEM_TRACKER_DISK_WARNING_THRESHOLD_AVAILABLE_SPACE: &str = "system-tracker-disk-warning-threshold-available-space";
/// `--system-tracker-frequency`.
pub const KEY_SYSTEM_TRACKER_FREQUENCY: &str = "system-tracker-frequency";
/// `--system-tracker-processing-halflife`.
pub const KEY_SYSTEM_TRACKER_PROCESSING_HALFLIFE: &str = "system-tracker-processing-halflife";
/// `--throttler-inbound-at-large-alloc-size`.
pub const KEY_THROTTLER_INBOUND_AT_LARGE_ALLOC_SIZE: &str = "throttler-inbound-at-large-alloc-size";
/// `--throttler-inbound-bandwidth-max-burst-size`.
pub const KEY_THROTTLER_INBOUND_BANDWIDTH_MAX_BURST_SIZE: &str = "throttler-inbound-bandwidth-max-burst-size";
/// `--throttler-inbound-bandwidth-refill-rate`.
pub const KEY_THROTTLER_INBOUND_BANDWIDTH_REFILL_RATE: &str = "throttler-inbound-bandwidth-refill-rate";
/// `--throttler-inbound-cpu-max-non-validator-node-usage`.
pub const KEY_THROTTLER_INBOUND_CPU_MAX_NON_VALIDATOR_NODE_USAGE: &str = "throttler-inbound-cpu-max-non-validator-node-usage";
/// `--throttler-inbound-cpu-max-non-validator-usage`.
pub const KEY_THROTTLER_INBOUND_CPU_MAX_NON_VALIDATOR_USAGE: &str = "throttler-inbound-cpu-max-non-validator-usage";
/// `--throttler-inbound-cpu-max-recheck-delay`.
pub const KEY_THROTTLER_INBOUND_CPU_MAX_RECHECK_DELAY: &str = "throttler-inbound-cpu-max-recheck-delay";
/// `--throttler-inbound-cpu-validator-alloc`.
pub const KEY_THROTTLER_INBOUND_CPU_VALIDATOR_ALLOC: &str = "throttler-inbound-cpu-validator-alloc";
/// `--throttler-inbound-disk-max-non-validator-node-usage`.
pub const KEY_THROTTLER_INBOUND_DISK_MAX_NON_VALIDATOR_NODE_USAGE: &str = "throttler-inbound-disk-max-non-validator-node-usage";
/// `--throttler-inbound-disk-max-non-validator-usage`.
pub const KEY_THROTTLER_INBOUND_DISK_MAX_NON_VALIDATOR_USAGE: &str = "throttler-inbound-disk-max-non-validator-usage";
/// `--throttler-inbound-disk-max-recheck-delay`.
pub const KEY_THROTTLER_INBOUND_DISK_MAX_RECHECK_DELAY: &str = "throttler-inbound-disk-max-recheck-delay";
/// `--throttler-inbound-disk-validator-alloc`.
pub const KEY_THROTTLER_INBOUND_DISK_VALIDATOR_ALLOC: &str = "throttler-inbound-disk-validator-alloc";
/// `--throttler-inbound-node-max-at-large-bytes`.
pub const KEY_THROTTLER_INBOUND_NODE_MAX_AT_LARGE_BYTES: &str = "throttler-inbound-node-max-at-large-bytes";
/// `--throttler-inbound-node-max-processing-msgs`.
pub const KEY_THROTTLER_INBOUND_NODE_MAX_PROCESSING_MSGS: &str = "throttler-inbound-node-max-processing-msgs";
/// `--throttler-inbound-validator-alloc-size`.
pub const KEY_THROTTLER_INBOUND_VALIDATOR_ALLOC_SIZE: &str = "throttler-inbound-validator-alloc-size";
/// `--throttler-outbound-at-large-alloc-size`.
pub const KEY_THROTTLER_OUTBOUND_AT_LARGE_ALLOC_SIZE: &str = "throttler-outbound-at-large-alloc-size";
/// `--throttler-outbound-node-max-at-large-bytes`.
pub const KEY_THROTTLER_OUTBOUND_NODE_MAX_AT_LARGE_BYTES: &str = "throttler-outbound-node-max-at-large-bytes";
/// `--throttler-outbound-validator-alloc-size`.
pub const KEY_THROTTLER_OUTBOUND_VALIDATOR_ALLOC_SIZE: &str = "throttler-outbound-validator-alloc-size";
/// `--tracing-endpoint`.
pub const KEY_TRACING_ENDPOINT: &str = "tracing-endpoint";
/// `--tracing-exporter-type`.
pub const KEY_TRACING_EXPORTER_TYPE: &str = "tracing-exporter-type";
/// `--tracing-headers`.
pub const KEY_TRACING_HEADERS: &str = "tracing-headers";
/// `--tracing-insecure`.
pub const KEY_TRACING_INSECURE: &str = "tracing-insecure";
/// `--tracing-sample-rate`.
pub const KEY_TRACING_SAMPLE_RATE: &str = "tracing-sample-rate";
/// `--track-subnets`.
pub const KEY_TRACK_SUBNETS: &str = "track-subnets";
/// `--tx-fee`.
pub const KEY_TX_FEE: &str = "tx-fee";
/// `--upgrade-file`.
pub const KEY_UPGRADE_FILE: &str = "upgrade-file";
/// `--upgrade-file-content`.
pub const KEY_UPGRADE_FILE_CONTENT: &str = "upgrade-file-content";
/// `--uptime-metric-freq`.
pub const KEY_UPTIME_METRIC_FREQ: &str = "uptime-metric-freq";
/// `--uptime-requirement`.
pub const KEY_UPTIME_REQUIREMENT: &str = "uptime-requirement";
/// `--validator-fees-capacity`.
pub const KEY_VALIDATOR_FEES_CAPACITY: &str = "validator-fees-capacity";
/// `--validator-fees-excess-conversion-constant`.
pub const KEY_VALIDATOR_FEES_EXCESS_CONVERSION_CONSTANT: &str = "validator-fees-excess-conversion-constant";
/// `--validator-fees-min-price`.
pub const KEY_VALIDATOR_FEES_MIN_PRICE: &str = "validator-fees-min-price";
/// `--validator-fees-target`.
pub const KEY_VALIDATOR_FEES_TARGET: &str = "validator-fees-target";
/// `--version`.
pub const KEY_VERSION: &str = "version";
/// `--version-json`.
pub const KEY_VERSION_JSON: &str = "version-json";
/// `--vm-aliases-file`.
pub const KEY_VM_ALIASES_FILE: &str = "vm-aliases-file";
/// `--vm-aliases-file-content`.
pub const KEY_VM_ALIASES_FILE_CONTENT: &str = "vm-aliases-file-content";

/// All 206 flag keys, sorted by name (specs 13 §24).
pub static ALL_KEYS: &[&str] = &[
    KEY_ACP_OBJECT,
    KEY_ACP_SUPPORT,
    KEY_API_ADMIN_ENABLED,
    KEY_API_HEALTH_ENABLED,
    KEY_API_INFO_ENABLED,
    KEY_API_METRICS_ENABLED,
    KEY_BENCHLIST_BENCH_PROBABILITY,
    KEY_BENCHLIST_DURATION,
    KEY_BENCHLIST_HALFLIFE,
    KEY_BENCHLIST_UNBENCH_PROBABILITY,
    KEY_BOOTSTRAP_ANCESTORS_MAX_CONTAINERS_RECEIVED,
    KEY_BOOTSTRAP_ANCESTORS_MAX_CONTAINERS_SENT,
    KEY_BOOTSTRAP_BEACON_CONNECTION_TIMEOUT,
    KEY_BOOTSTRAP_IDS,
    KEY_BOOTSTRAP_IPS,
    KEY_BOOTSTRAP_MAX_TIME_GET_ANCESTORS,
    KEY_CHAIN_ALIASES_FILE,
    KEY_CHAIN_ALIASES_FILE_CONTENT,
    KEY_CHAIN_CONFIG_CONTENT,
    KEY_CHAIN_CONFIG_DIR,
    KEY_CHAIN_DATA_DIR,
    KEY_CONFIG_FILE,
    KEY_CONFIG_FILE_CONTENT,
    KEY_CONFIG_FILE_CONTENT_TYPE,
    KEY_CONSENSUS_APP_CONCURRENCY,
    KEY_CONSENSUS_FRONTIER_POLL_FREQUENCY,
    KEY_CONSENSUS_SHUTDOWN_TIMEOUT,
    KEY_CREATE_ASSET_TX_FEE,
    KEY_DATA_DIR,
    KEY_DB_CONFIG_FILE,
    KEY_DB_CONFIG_FILE_CONTENT,
    KEY_DB_DIR,
    KEY_DB_READ_ONLY,
    KEY_DB_TYPE,
    KEY_DYNAMIC_FEES_BANDWIDTH_WEIGHT,
    KEY_DYNAMIC_FEES_COMPUTE_WEIGHT,
    KEY_DYNAMIC_FEES_DB_READ_WEIGHT,
    KEY_DYNAMIC_FEES_DB_WRITE_WEIGHT,
    KEY_DYNAMIC_FEES_EXCESS_CONVERSION_CONSTANT,
    KEY_DYNAMIC_FEES_MAX_GAS_CAPACITY,
    KEY_DYNAMIC_FEES_MAX_GAS_PER_SECOND,
    KEY_DYNAMIC_FEES_MIN_GAS_PRICE,
    KEY_DYNAMIC_FEES_TARGET_GAS_PER_SECOND,
    KEY_FD_LIMIT,
    KEY_GENESIS_FILE,
    KEY_GENESIS_FILE_CONTENT,
    KEY_HEALTH_CHECK_AVERAGER_HALFLIFE,
    KEY_HEALTH_CHECK_FREQUENCY,
    KEY_HTTP_ALLOWED_HOSTS,
    KEY_HTTP_ALLOWED_ORIGINS,
    KEY_HTTP_HOST,
    KEY_HTTP_IDLE_TIMEOUT,
    KEY_HTTP_PORT,
    KEY_HTTP_READ_HEADER_TIMEOUT,
    KEY_HTTP_READ_TIMEOUT,
    KEY_HTTP_SHUTDOWN_TIMEOUT,
    KEY_HTTP_SHUTDOWN_WAIT,
    KEY_HTTP_TLS_CERT_FILE,
    KEY_HTTP_TLS_CERT_FILE_CONTENT,
    KEY_HTTP_TLS_ENABLED,
    KEY_HTTP_TLS_KEY_FILE,
    KEY_HTTP_TLS_KEY_FILE_CONTENT,
    KEY_HTTP_WRITE_TIMEOUT,
    KEY_INDEX_ALLOW_INCOMPLETE,
    KEY_INDEX_ENABLED,
    KEY_LOG_DIR,
    KEY_LOG_DISABLE_DISPLAY_PLUGIN_LOGS,
    KEY_LOG_DISPLAY_LEVEL,
    KEY_LOG_FORMAT,
    KEY_LOG_LEVEL,
    KEY_LOG_ROTATER_COMPRESS_ENABLED,
    KEY_LOG_ROTATER_MAX_AGE,
    KEY_LOG_ROTATER_MAX_FILES,
    KEY_LOG_ROTATER_MAX_SIZE,
    KEY_MAX_STAKE_DURATION,
    KEY_MAX_VALIDATOR_STAKE,
    KEY_METER_VMS_ENABLED,
    KEY_MIN_DELEGATION_FEE,
    KEY_MIN_DELEGATOR_STAKE,
    KEY_MIN_STAKE_DURATION,
    KEY_MIN_VALIDATOR_STAKE,
    KEY_NETWORK_ALLOW_PRIVATE_IPS,
    KEY_NETWORK_COMPRESSION_TYPE,
    KEY_NETWORK_HEALTH_MAX_OUTSTANDING_REQUEST_DURATION,
    KEY_NETWORK_HEALTH_MAX_PORTION_SEND_QUEUE_FULL,
    KEY_NETWORK_HEALTH_MAX_SEND_FAIL_RATE,
    KEY_NETWORK_HEALTH_MAX_TIME_SINCE_MSG_RECEIVED,
    KEY_NETWORK_HEALTH_MAX_TIME_SINCE_MSG_SENT,
    KEY_NETWORK_HEALTH_MIN_CONN_PEERS,
    KEY_NETWORK_ID,
    KEY_NETWORK_INBOUND_CONNECTION_THROTTLING_COOLDOWN,
    KEY_NETWORK_INBOUND_CONNECTION_THROTTLING_MAX_CONNS_PER_SEC,
    KEY_NETWORK_INITIAL_RECONNECT_DELAY,
    KEY_NETWORK_INITIAL_TIMEOUT,
    KEY_NETWORK_MAX_CLOCK_DIFFERENCE,
    KEY_NETWORK_MAX_RECONNECT_DELAY,
    KEY_NETWORK_MAXIMUM_INBOUND_TIMEOUT,
    KEY_NETWORK_MAXIMUM_TIMEOUT,
    KEY_NETWORK_MINIMUM_TIMEOUT,
    KEY_NETWORK_NO_INGRESS_CONNECTIONS_GRACE_PERIOD,
    KEY_NETWORK_OUTBOUND_CONNECTION_THROTTLING_RPS,
    KEY_NETWORK_OUTBOUND_CONNECTION_TIMEOUT,
    KEY_NETWORK_PEER_LIST_BLOOM_RESET_FREQUENCY,
    KEY_NETWORK_PEER_LIST_NUM_VALIDATOR_IPS,
    KEY_NETWORK_PEER_LIST_PULL_GOSSIP_FREQUENCY,
    KEY_NETWORK_PEER_READ_BUFFER_SIZE,
    KEY_NETWORK_PEER_WRITE_BUFFER_SIZE,
    KEY_NETWORK_PING_FREQUENCY,
    KEY_NETWORK_PING_TIMEOUT,
    KEY_NETWORK_READ_HANDSHAKE_TIMEOUT,
    KEY_NETWORK_REQUIRE_VALIDATOR_TO_CONNECT,
    KEY_NETWORK_TCP_PROXY_ENABLED,
    KEY_NETWORK_TCP_PROXY_READ_TIMEOUT,
    KEY_NETWORK_TIMEOUT_COEFFICIENT,
    KEY_NETWORK_TIMEOUT_HALFLIFE,
    KEY_NETWORK_TLS_KEY_LOG_FILE_UNSAFE,
    KEY_PARTIAL_SYNC_PRIMARY_NETWORK,
    KEY_PLUGIN_DIR,
    KEY_PROCESS_CONTEXT_FILE,
    KEY_PROFILE_CONTINUOUS_ENABLED,
    KEY_PROFILE_CONTINUOUS_FREQ,
    KEY_PROFILE_CONTINUOUS_MAX_FILES,
    KEY_PROFILE_DIR,
    KEY_PROPOSERVM_MIN_BLOCK_DELAY,
    KEY_PROPOSERVM_USE_CURRENT_HEIGHT,
    KEY_PUBLIC_IP,
    KEY_PUBLIC_IP_RESOLUTION_FREQUENCY,
    KEY_PUBLIC_IP_RESOLUTION_SERVICE,
    KEY_ROUTER_HEALTH_MAX_DROP_RATE,
    KEY_ROUTER_HEALTH_MAX_OUTSTANDING_REQUESTS,
    KEY_SIMPLEX_MAX_NETWORK_DELAY,
    KEY_SIMPLEX_MAX_REBROADCAST_WAIT,
    KEY_SNOW_COMMIT_THRESHOLD,
    KEY_SNOW_CONCURRENT_REPOLLS,
    KEY_SNOW_CONFIDENCE_QUORUM_SIZE,
    KEY_SNOW_MAX_PROCESSING,
    KEY_SNOW_MAX_TIME_PROCESSING,
    KEY_SNOW_OPTIMAL_PROCESSING,
    KEY_SNOW_PREFERENCE_QUORUM_SIZE,
    KEY_SNOW_QUORUM_SIZE,
    KEY_SNOW_SAMPLE_SIZE,
    KEY_STAKE_MAX_CONSUMPTION_RATE,
    KEY_STAKE_MIN_CONSUMPTION_RATE,
    KEY_STAKE_MINTING_PERIOD,
    KEY_STAKE_SUPPLY_CAP,
    KEY_STAKING_EPHEMERAL_CERT_ENABLED,
    KEY_STAKING_EPHEMERAL_SIGNER_ENABLED,
    KEY_STAKING_HOST,
    KEY_STAKING_PORT,
    KEY_STAKING_RPC_SIGNER_ENDPOINT,
    KEY_STAKING_SIGNER_KEY_FILE,
    KEY_STAKING_SIGNER_KEY_FILE_CONTENT,
    KEY_STAKING_TLS_CERT_FILE,
    KEY_STAKING_TLS_CERT_FILE_CONTENT,
    KEY_STAKING_TLS_KEY_FILE,
    KEY_STAKING_TLS_KEY_FILE_CONTENT,
    KEY_STATE_SYNC_IDS,
    KEY_STATE_SYNC_IPS,
    KEY_SUBNET_CONFIG_CONTENT,
    KEY_SUBNET_CONFIG_DIR,
    KEY_SYBIL_PROTECTION_DISABLED_WEIGHT,
    KEY_SYBIL_PROTECTION_ENABLED,
    KEY_SYSTEM_TRACKER_CPU_HALFLIFE,
    KEY_SYSTEM_TRACKER_DISK_HALFLIFE,
    KEY_SYSTEM_TRACKER_DISK_REQUIRED_AVAILABLE_SPACE,
    KEY_SYSTEM_TRACKER_DISK_REQUIRED_AVAILABLE_SPACE_PERCENTAGE,
    KEY_SYSTEM_TRACKER_DISK_WARNING_AVAILABLE_SPACE_PERCENTAGE,
    KEY_SYSTEM_TRACKER_DISK_WARNING_THRESHOLD_AVAILABLE_SPACE,
    KEY_SYSTEM_TRACKER_FREQUENCY,
    KEY_SYSTEM_TRACKER_PROCESSING_HALFLIFE,
    KEY_THROTTLER_INBOUND_AT_LARGE_ALLOC_SIZE,
    KEY_THROTTLER_INBOUND_BANDWIDTH_MAX_BURST_SIZE,
    KEY_THROTTLER_INBOUND_BANDWIDTH_REFILL_RATE,
    KEY_THROTTLER_INBOUND_CPU_MAX_NON_VALIDATOR_NODE_USAGE,
    KEY_THROTTLER_INBOUND_CPU_MAX_NON_VALIDATOR_USAGE,
    KEY_THROTTLER_INBOUND_CPU_MAX_RECHECK_DELAY,
    KEY_THROTTLER_INBOUND_CPU_VALIDATOR_ALLOC,
    KEY_THROTTLER_INBOUND_DISK_MAX_NON_VALIDATOR_NODE_USAGE,
    KEY_THROTTLER_INBOUND_DISK_MAX_NON_VALIDATOR_USAGE,
    KEY_THROTTLER_INBOUND_DISK_MAX_RECHECK_DELAY,
    KEY_THROTTLER_INBOUND_DISK_VALIDATOR_ALLOC,
    KEY_THROTTLER_INBOUND_NODE_MAX_AT_LARGE_BYTES,
    KEY_THROTTLER_INBOUND_NODE_MAX_PROCESSING_MSGS,
    KEY_THROTTLER_INBOUND_VALIDATOR_ALLOC_SIZE,
    KEY_THROTTLER_OUTBOUND_AT_LARGE_ALLOC_SIZE,
    KEY_THROTTLER_OUTBOUND_NODE_MAX_AT_LARGE_BYTES,
    KEY_THROTTLER_OUTBOUND_VALIDATOR_ALLOC_SIZE,
    KEY_TRACING_ENDPOINT,
    KEY_TRACING_EXPORTER_TYPE,
    KEY_TRACING_HEADERS,
    KEY_TRACING_INSECURE,
    KEY_TRACING_SAMPLE_RATE,
    KEY_TRACK_SUBNETS,
    KEY_TX_FEE,
    KEY_UPGRADE_FILE,
    KEY_UPGRADE_FILE_CONTENT,
    KEY_UPTIME_METRIC_FREQ,
    KEY_UPTIME_REQUIREMENT,
    KEY_VALIDATOR_FEES_CAPACITY,
    KEY_VALIDATOR_FEES_EXCESS_CONVERSION_CONSTANT,
    KEY_VALIDATOR_FEES_MIN_PRICE,
    KEY_VALIDATOR_FEES_TARGET,
    KEY_VERSION,
    KEY_VERSION_JSON,
    KEY_VM_ALIASES_FILE,
    KEY_VM_ALIASES_FILE_CONTENT,
];

#[cfg(test)]
mod tests {
    #[test]
    fn key_count_matches_go() {
        // 13 §24: 205 keys in the keys.go const block + http-write-timeout.
        assert_eq!(super::ALL_KEYS.len(), 206);
    }

    #[test]
    fn keys_are_sorted_and_unique() {
        assert!(super::ALL_KEYS.windows(2).all(|w| w[0] < w[1]));
    }
}
