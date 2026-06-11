// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `get_node_config` — the order-sensitive resolution of the layered flags
//! into the node [`Config`] (Go `config/config.go::GetNodeConfig`,
//! specs 12 §1.6, 13 §3/§5/§7/§8/§13/§18/§19/§21).
//!
//! Every helper mirrors its Go counterpart (`getXxxConfig`) in field and
//! validation-branch order. Go validates many `time.Duration` flags as
//! `>= 0`; `std::time::Duration` is unsigned so those checks are vacuous here
//! and elided (the `> 0` checks are kept as `is_zero()` tests).

use std::collections::{BTreeMap, BTreeSet};
use std::io::IsTerminal as _;
use std::net::SocketAddr;
use std::path::Path;
use std::str::FromStr as _;
use std::time::Duration;

use ava_engine::networking::timeout::AdaptiveTimeoutConfig;
use ava_engine::networking::tracker::TargeterConfig;
use ava_genesis::Bootstrapper;
use ava_genesis::params::{
    self as genesis_params, DynamicFeeConfig, PERCENT_DENOMINATOR, RewardConfig,
    StakingConfig as GenesisStakingConfig, TxFeeConfig, ValidatorFeeConfig,
};
use ava_logging::{AvaLevel, Format};
use ava_network::identity::Identity;
use ava_types::constants::{FUJI_ID, LOCAL_ID, MAINNET_ID, VALID_NETWORK_PREFIX, network_name};
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use base64::Engine as _;

use crate::ConfigError;
use crate::chain_config;
use crate::flags::{FLAG_SPECS, FlagKind};
use crate::keys;
use crate::node::{
    ApiConfig, BenchlistConfig, BootstrapConfig, Config, DatabaseConfig, HttpConfig, IpConfig,
    LoggingConfig, NetworkConfig, ProfilerConfig, RouterHealthConfig, StakingConfig,
    StakingSignerConfig, StateSyncConfig, TraceConfig, TraceExporterType,
};
use crate::precedence::Layered;
use crate::subnets::{self, PRIMARY_NETWORK_ID, SnowParameters};

/// Go `constants.ActivatedACPs` (`utils/constants/acps.go`): Durango + Etna +
/// Fortuna + Granite ACPs. Peers are not notified of opinions on these.
const ACTIVATED_ACPS: &[u32] = &[
    // Durango:
    23, 24, 25, 30, 31, 41, 62, //
    // Etna:
    77, 103, 118, 125, 131, 151, //
    // Fortuna:
    176, //
    // Granite:
    181, 204, 226,
];

/// Go `constants.ScheduledACPs` — the ACPs included in the next upgrade
/// (currently empty).
const SCHEDULED_ACPS: &[u32] = &[];

/// Go `config.maxDiskSpaceThreshold` (13 §18).
const MAX_DISK_SPACE_THRESHOLD: u64 = 50;

/// Go `constants.MinInboundThrottlerMaxRecheckDelay`
/// (`utils/constants/networking.go`).
const MIN_INBOUND_THROTTLER_MAX_RECHECK_DELAY: Duration = Duration::from_millis(1);

/// Shorthand for the non-sentinel `fmt.Errorf`-shaped failures.
fn invalid(key: &str, msg: impl Into<String>) -> ConfigError {
    ConfigError::InvalidValue {
        key: key.to_string(),
        msg: msg.into(),
    }
}

/// Reads a `uint`-kinded flag narrowed to `u32` (Go `int(v.GetUint(..))`).
fn get_u32(layered: &Layered, key: &str) -> crate::Result<u32> {
    let v = layered.get_u64(key)?;
    u32::try_from(v).map_err(|_| invalid(key, format!("({v}) must be in [0, {}]", u32::MAX)))
}

/// Reads a port-sized flag (Go `uint16(v.GetUint(..))`).
fn get_u16(layered: &Layered, key: &str) -> crate::Result<u16> {
    let v = layered.get_u64(key)?;
    u16::try_from(v).map_err(|_| invalid(key, format!("({v}) must be in [0, {}]", u16::MAX)))
}

/// Go `constants.NetworkID` — a known network name, `network-<n>`, or a bare
/// number.
fn parse_network_id(name: &str) -> crate::Result<u32> {
    let lower = name.to_lowercase();
    if let Some(id) = ava_types::constants::network_id(&lower) {
        return Ok(id);
    }
    let id_str = lower.strip_prefix(VALID_NETWORK_PREFIX).unwrap_or(&lower);
    id_str
        .parse::<u32>()
        .map_err(|_| ConfigError::InvalidNetworkId {
            name: name.to_string(),
        })
}

/// Go `getPluginDir` — expanded; asserted to be an existing directory when
/// explicitly set, created when defaulted.
fn get_plugin_dir(layered: &Layered) -> crate::Result<String> {
    let plugin_dir = layered.get_expanded_string(keys::KEY_PLUGIN_DIR)?;
    if layered.is_set(keys::KEY_PLUGIN_DIR) {
        let info = std::fs::metadata(&plugin_dir).map_err(|e| {
            invalid(
                keys::KEY_PLUGIN_DIR,
                format!("not found: {plugin_dir:?}: {e}"),
            )
        })?;
        if !info.is_dir() {
            return Err(invalid(
                keys::KEY_PLUGIN_DIR,
                format!("plugin dir is not a directory: {plugin_dir:?}"),
            ));
        }
    } else {
        std::fs::create_dir_all(&plugin_dir).map_err(|e| {
            invalid(
                keys::KEY_PLUGIN_DIR,
                format!("failed to create plugin dir at {plugin_dir}: {e}"),
            )
        })?;
    }
    Ok(plugin_dir)
}

/// Go `logging.ToLevel`.
fn parse_level(key: &str, s: &str) -> crate::Result<AvaLevel> {
    AvaLevel::from_str(s).map_err(|e| invalid(key, e.to_string()))
}

/// Go `logging.ToFormat(s, os.Stdout.Fd())` — `auto` resolves against the tty.
fn parse_log_format(s: &str) -> crate::Result<Format> {
    match s.to_lowercase().as_str() {
        "auto" => {
            if std::io::stdout().is_terminal() {
                Ok(Format::Colors)
            } else {
                Ok(Format::Plain)
            }
        }
        "plain" => Ok(Format::Plain),
        "colors" => Ok(Format::Colors),
        "json" => Ok(Format::Json),
        other => Err(invalid(
            keys::KEY_LOG_FORMAT,
            format!("unknown format mode: {other}"),
        )),
    }
}

/// Go `getLoggingConfig`.
fn get_logging_config(layered: &Layered) -> crate::Result<LoggingConfig> {
    let directory = layered.get_expanded_string(keys::KEY_LOG_DIR)?;
    let log_level_str = layered.get_string(keys::KEY_LOG_LEVEL)?;
    let log_level = parse_level(keys::KEY_LOG_LEVEL, &log_level_str)?;
    let display_str = if layered.is_set(keys::KEY_LOG_DISPLAY_LEVEL) {
        layered.get_string(keys::KEY_LOG_DISPLAY_LEVEL)?
    } else {
        log_level_str
    };
    let display_level = parse_level(keys::KEY_LOG_DISPLAY_LEVEL, &display_str)?;
    let log_format = parse_log_format(&layered.get_string(keys::KEY_LOG_FORMAT)?)?;
    Ok(LoggingConfig {
        directory,
        log_level,
        display_level,
        log_format,
        disable_writer_displaying: layered.get_bool(keys::KEY_LOG_DISABLE_DISPLAY_PLUGIN_LOGS)?,
        max_size: get_u32(layered, keys::KEY_LOG_ROTATER_MAX_SIZE)?,
        max_files: get_u32(layered, keys::KEY_LOG_ROTATER_MAX_FILES)?,
        max_age: get_u32(layered, keys::KEY_LOG_ROTATER_MAX_AGE)?,
        compress: layered.get_bool(keys::KEY_LOG_ROTATER_COMPRESS_ENABLED)?,
    })
}

/// Decodes a `-content` flag's base64 payload to raw bytes.
fn decode_b64(b64: &str) -> crate::Result<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(b64.trim())
        .map_err(|e| ConfigError::InvalidBase64Content { msg: e.to_string() })
}

/// Reads a file referenced by a path flag, mapping I/O failures.
fn read_file(path: &str) -> crate::Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| ConfigError::ConfigFileRead {
        path: path.to_string(),
        msg: e.to_string(),
    })
}

/// Go `getDatabaseConfig` — db type/read-only/`<db-dir>/<networkName>` path +
/// the optional raw config blob.
fn get_database_config(layered: &Layered, network_id: u32) -> crate::Result<DatabaseConfig> {
    let config = if layered.is_set(keys::KEY_DB_CONFIG_FILE_CONTENT) {
        decode_b64(&layered.get_string(keys::KEY_DB_CONFIG_FILE_CONTENT)?)?
    } else if layered.is_set(keys::KEY_DB_CONFIG_FILE) {
        read_file(&layered.get_expanded_string(keys::KEY_DB_CONFIG_FILE)?)?
    } else {
        Vec::new()
    };
    let path = Path::new(&layered.get_expanded_string(keys::KEY_DB_DIR)?)
        .join(network_name(network_id))
        .display()
        .to_string();
    Ok(DatabaseConfig {
        name: layered.get_string(keys::KEY_DB_TYPE)?,
        read_only: layered.get_bool(keys::KEY_DB_READ_ONLY)?,
        path,
        config,
    })
}

/// Go `getIPConfig` — resolution frequency must be positive; `--public-ip`
/// and `--public-ip-resolution-service` are mutually exclusive (13 §19).
fn get_ip_config(layered: &Layered) -> crate::Result<IpConfig> {
    let ip_config = IpConfig {
        public_ip: layered.get_string(keys::KEY_PUBLIC_IP)?,
        public_ip_resolution_service: layered.get_string(keys::KEY_PUBLIC_IP_RESOLUTION_SERVICE)?,
        public_ip_resolution_freq: layered
            .get_duration(keys::KEY_PUBLIC_IP_RESOLUTION_FREQUENCY)?,
        listen_host: layered.get_string(keys::KEY_STAKING_HOST)?,
        listen_port: get_u16(layered, keys::KEY_STAKING_PORT)?,
    };
    if ip_config.public_ip_resolution_freq.is_zero() {
        return Err(invalid(
            keys::KEY_PUBLIC_IP_RESOLUTION_FREQUENCY,
            "must be > 0",
        ));
    }
    if !ip_config.public_ip.is_empty() && !ip_config.public_ip_resolution_service.is_empty() {
        return Err(ConfigError::ConflictingPublicIpOptions);
    }
    Ok(ip_config)
}

/// Maps an `ava-network` identity error to the staking-cert sentinel.
fn cert_err(prefix: &str, e: impl std::fmt::Display) -> ConfigError {
    ConfigError::StakingCert {
        msg: format!("{prefix}: {e}"),
    }
}

/// Go `getStakingTLSCertFromFlag` — both `-content` flags, base64 PEM.
fn staking_identity_from_content(layered: &Layered) -> crate::Result<Identity> {
    let key_pem = decode_b64(&layered.get_string(keys::KEY_STAKING_TLS_KEY_FILE_CONTENT)?)?;
    let cert_pem = decode_b64(&layered.get_string(keys::KEY_STAKING_TLS_CERT_FILE_CONTENT)?)?;
    let key_pem = String::from_utf8(key_pem)
        .map_err(|e| cert_err("failed creating cert", format!("key is not PEM text: {e}")))?;
    let cert_pem = String::from_utf8(cert_pem)
        .map_err(|e| cert_err("failed creating cert", format!("cert is not PEM text: {e}")))?;
    Identity::from_pem(&cert_pem, &key_pem).map_err(|e| cert_err("failed creating cert", e))
}

/// Go `staking.InitNodeStakingKeyPair` — generate + write the key/cert pair
/// (no-op if the key file already exists; Go parity).
fn init_node_staking_key_pair(key_path: &str, cert_path: &str) -> crate::Result<()> {
    if Path::new(key_path).exists() {
        return Ok(());
    }
    let (cert_pem, key_pem) = ava_crypto::staking::new_cert_and_key_bytes()
        .map_err(|e| cert_err("couldn't generate staking key/cert", e))?;
    ava_crypto::staking::write_cert_and_key(
        Path::new(cert_path),
        Path::new(key_path),
        &cert_pem,
        &key_pem,
    )
    .map_err(|e| cert_err("couldn't write staking key/cert", e))
}

/// Go `getStakingTLSCertFromFile` — explicit paths must exist; defaulted paths
/// are created on first run.
fn staking_identity_from_files(layered: &Layered) -> crate::Result<Identity> {
    let key_path = layered.get_expanded_string(keys::KEY_STAKING_TLS_KEY_FILE)?;
    let cert_path = layered.get_expanded_string(keys::KEY_STAKING_TLS_CERT_FILE)?;

    if layered.is_set(keys::KEY_STAKING_TLS_KEY_FILE)
        || layered.is_set(keys::KEY_STAKING_TLS_CERT_FILE)
    {
        if !Path::new(&key_path).exists() {
            return Err(ConfigError::StakingCert {
                msg: format!("couldn't find staking key at {key_path}"),
            });
        }
        if !Path::new(&cert_path).exists() {
            return Err(ConfigError::StakingCert {
                msg: format!("couldn't find staking certificate at {cert_path}"),
            });
        }
    } else {
        init_node_staking_key_pair(&key_path, &cert_path)?;
    }

    let key_pem = std::fs::read_to_string(&key_path)
        .map_err(|e| cert_err("couldn't read staking certificate", e))?;
    let cert_pem = std::fs::read_to_string(&cert_path)
        .map_err(|e| cert_err("couldn't read staking certificate", e))?;
    Identity::from_pem(&cert_pem, &key_pem)
        .map_err(|e| cert_err("couldn't read staking certificate", e))
}

/// Go `getStakingTLSCert` — ephemeral, content, or file (in that order).
fn get_staking_tls_identity(layered: &Layered) -> crate::Result<Identity> {
    if layered.get_bool(keys::KEY_STAKING_EPHEMERAL_CERT_ENABLED)? {
        return Identity::generate()
            .map_err(|e| cert_err("couldn't generate ephemeral staking key/cert", e));
    }
    let key_set = layered.is_set(keys::KEY_STAKING_TLS_KEY_FILE_CONTENT);
    let cert_set = layered.is_set(keys::KEY_STAKING_TLS_CERT_FILE_CONTENT);
    match (key_set, cert_set) {
        (true, false) => Err(ConfigError::StakingCertContentUnset),
        (false, true) => Err(ConfigError::StakingKeyContentUnset),
        (true, true) => staking_identity_from_content(layered),
        (false, false) => staking_identity_from_files(layered),
    }
}

/// Go `getStakingSignerConfig` — at most one signer option may be set.
fn get_staking_signer_config(layered: &Layered) -> crate::Result<StakingSignerConfig> {
    let ephemeral_signer_enabled = layered.get_bool(keys::KEY_STAKING_EPHEMERAL_SIGNER_ENABLED)?;
    let content_set = layered.is_set(keys::KEY_STAKING_SIGNER_KEY_FILE_CONTENT);
    let key_path_is_set = layered.is_set(keys::KEY_STAKING_SIGNER_KEY_FILE);
    let rpc_set = layered.is_set(keys::KEY_STAKING_RPC_SIGNER_ENDPOINT);
    let num_set = usize::from(ephemeral_signer_enabled)
        .saturating_add(usize::from(content_set))
        .saturating_add(usize::from(key_path_is_set))
        .saturating_add(usize::from(rpc_set));
    if num_set > 1 {
        return Err(ConfigError::InvalidSignerConfig);
    }

    // The key path applies only when no other signer option is set.
    let key_path = if !ephemeral_signer_enabled && !content_set && !rpc_set {
        layered.get_expanded_string(keys::KEY_STAKING_SIGNER_KEY_FILE)?
    } else {
        String::new()
    };

    Ok(StakingSignerConfig {
        ephemeral_signer_enabled,
        key_content: layered.get_expanded_string(keys::KEY_STAKING_SIGNER_KEY_FILE_CONTENT)?,
        key_path,
        rpc_endpoint: layered.get_expanded_string(keys::KEY_STAKING_RPC_SIGNER_ENDPOINT)?,
        key_path_is_set,
    })
}

/// The staking-economics block from the flags (custom networks only), with
/// Go `getStakingConfig`'s validation branch order.
fn staking_economics_from_flags(layered: &Layered) -> crate::Result<GenesisStakingConfig> {
    let uptime_requirement = layered.get_f64(keys::KEY_UPTIME_REQUIREMENT)?;
    let min_validator_stake = layered.get_u64(keys::KEY_MIN_VALIDATOR_STAKE)?;
    let max_validator_stake = layered.get_u64(keys::KEY_MAX_VALIDATOR_STAKE)?;
    let min_delegator_stake = layered.get_u64(keys::KEY_MIN_DELEGATOR_STAKE)?;
    let min_stake_duration = layered.get_duration(keys::KEY_MIN_STAKE_DURATION)?;
    let max_stake_duration = layered.get_duration(keys::KEY_MAX_STAKE_DURATION)?;
    let max_consumption_rate = layered.get_u64(keys::KEY_STAKE_MAX_CONSUMPTION_RATE)?;
    let min_consumption_rate = layered.get_u64(keys::KEY_STAKE_MIN_CONSUMPTION_RATE)?;
    let minting_period = layered.get_duration(keys::KEY_STAKE_MINTING_PERIOD)?;
    let supply_cap = layered.get_u64(keys::KEY_STAKE_SUPPLY_CAP)?;
    let min_delegation_fee = layered.get_u64(keys::KEY_MIN_DELEGATION_FEE)?;

    if !(0.0..=1.0).contains(&uptime_requirement) {
        return Err(ConfigError::InvalidUptimeRequirement);
    }
    if min_validator_stake > max_validator_stake {
        return Err(ConfigError::MinValidatorStakeAboveMax);
    }
    if min_delegation_fee > 1_000_000 {
        return Err(ConfigError::InvalidDelegationFee);
    }
    if min_stake_duration.is_zero() {
        return Err(ConfigError::InvalidMinStakeDuration);
    }
    if max_stake_duration < min_stake_duration {
        return Err(ConfigError::MinStakeDurationAboveMax);
    }
    if max_consumption_rate > PERCENT_DENOMINATOR {
        return Err(ConfigError::StakeMaxConsumptionTooLarge);
    }
    if max_consumption_rate < min_consumption_rate {
        return Err(ConfigError::StakeMaxConsumptionBelowMin);
    }
    if minting_period < max_stake_duration {
        return Err(ConfigError::StakeMintingPeriodBelowMin);
    }

    let minting_period_nanos = u64::try_from(minting_period.as_nanos())
        .map_err(|_| invalid(keys::KEY_STAKE_MINTING_PERIOD, "overflows u64 nanoseconds"))?;
    // Bounds-checked against 1_000_000 above.
    let min_delegation_fee =
        u32::try_from(min_delegation_fee).map_err(|_| ConfigError::InvalidDelegationFee)?;
    Ok(GenesisStakingConfig {
        uptime_requirement,
        min_validator_stake,
        max_validator_stake,
        min_delegator_stake,
        min_delegation_fee,
        min_stake_duration,
        max_stake_duration,
        reward_config: RewardConfig {
            max_consumption_rate,
            min_consumption_rate,
            minting_period: minting_period_nanos,
            supply_cap,
        },
    })
}

/// Go `getStakingConfig` — sybil checks, TLS cert, signer, and the
/// staking-economics block (flags on custom networks, genesis params on
/// Mainnet/Fuji; 13 §5).
fn get_staking_config(layered: &Layered, network_id: u32) -> crate::Result<StakingConfig> {
    let sybil_protection_enabled = layered.get_bool(keys::KEY_SYBIL_PROTECTION_ENABLED)?;
    let sybil_protection_disabled_weight =
        layered.get_u64(keys::KEY_SYBIL_PROTECTION_DISABLED_WEIGHT)?;

    if !sybil_protection_enabled && sybil_protection_disabled_weight == 0 {
        return Err(ConfigError::SybilProtectionDisabledStakerWeights);
    }
    if !sybil_protection_enabled && (network_id == MAINNET_ID || network_id == FUJI_ID) {
        return Err(ConfigError::SybilProtectionDisabledOnPublicNetwork);
    }

    let identity = get_staking_tls_identity(layered)?;
    let signer = get_staking_signer_config(layered)?;

    let economics = if network_id != MAINNET_ID && network_id != FUJI_ID {
        staking_economics_from_flags(layered)?
    } else {
        genesis_params::get_staking_config(network_id)
    };

    Ok(StakingConfig {
        sybil_protection_enabled,
        sybil_protection_disabled_weight,
        partial_sync_primary_network: layered.get_bool(keys::KEY_PARTIAL_SYNC_PRIMARY_NETWORK)?,
        staking_tls_key_path: layered.get_expanded_string(keys::KEY_STAKING_TLS_KEY_FILE)?,
        staking_tls_cert_path: layered.get_expanded_string(keys::KEY_STAKING_TLS_CERT_FILE)?,
        identity,
        signer,
        economics,
    })
}

/// Go `getTrackedSubnets` — comma-separated subnet IDs; the Primary Network
/// may not be tracked.
fn get_tracked_subnets(layered: &Layered) -> crate::Result<BTreeSet<Id>> {
    let raw = layered.get_string(keys::KEY_TRACK_SUBNETS)?;
    let mut tracked = BTreeSet::new();
    for part in raw.split(',') {
        if part.is_empty() {
            continue;
        }
        let subnet_id = Id::from_str(part).map_err(|e| {
            invalid(
                keys::KEY_TRACK_SUBNETS,
                format!("couldn't parse subnetID {part:?}: {e}"),
            )
        })?;
        if subnet_id == PRIMARY_NETWORK_ID {
            return Err(ConfigError::CannotTrackPrimaryNetwork);
        }
        tracked.insert(subnet_id);
    }
    Ok(tracked)
}

/// Go `getHTTPConfig` (+ the API enable block).
fn get_http_config(layered: &Layered) -> crate::Result<HttpConfig> {
    let https_key = if layered.is_set(keys::KEY_HTTP_TLS_KEY_FILE_CONTENT) {
        decode_b64(&layered.get_string(keys::KEY_HTTP_TLS_KEY_FILE_CONTENT)?)?
    } else if layered.is_set(keys::KEY_HTTP_TLS_KEY_FILE) {
        read_file(&layered.get_expanded_string(keys::KEY_HTTP_TLS_KEY_FILE)?)?
    } else {
        Vec::new()
    };
    let https_cert = if layered.is_set(keys::KEY_HTTP_TLS_CERT_FILE_CONTENT) {
        decode_b64(&layered.get_string(keys::KEY_HTTP_TLS_CERT_FILE_CONTENT)?)?
    } else if layered.is_set(keys::KEY_HTTP_TLS_CERT_FILE) {
        read_file(&layered.get_expanded_string(keys::KEY_HTTP_TLS_CERT_FILE)?)?
    } else {
        Vec::new()
    };

    Ok(HttpConfig {
        read_timeout: layered.get_duration(keys::KEY_HTTP_READ_TIMEOUT)?,
        read_header_timeout: layered.get_duration(keys::KEY_HTTP_READ_HEADER_TIMEOUT)?,
        write_timeout: layered.get_duration(keys::KEY_HTTP_WRITE_TIMEOUT)?,
        idle_timeout: layered.get_duration(keys::KEY_HTTP_IDLE_TIMEOUT)?,
        api_config: ApiConfig {
            index_api_enabled: layered.get_bool(keys::KEY_INDEX_ENABLED)?,
            index_allow_incomplete: layered.get_bool(keys::KEY_INDEX_ALLOW_INCOMPLETE)?,
            admin_api_enabled: layered.get_bool(keys::KEY_API_ADMIN_ENABLED)?,
            info_api_enabled: layered.get_bool(keys::KEY_API_INFO_ENABLED)?,
            metrics_api_enabled: layered.get_bool(keys::KEY_API_METRICS_ENABLED)?,
            health_api_enabled: layered.get_bool(keys::KEY_API_HEALTH_ENABLED)?,
        },
        http_host: layered.get_string(keys::KEY_HTTP_HOST)?,
        http_port: get_u16(layered, keys::KEY_HTTP_PORT)?,
        https_enabled: layered.get_bool(keys::KEY_HTTP_TLS_ENABLED)?,
        https_key,
        https_cert,
        http_allowed_origins: layered.get_string_slice(keys::KEY_HTTP_ALLOWED_ORIGINS)?,
        http_allowed_hosts: layered.get_string_slice(keys::KEY_HTTP_ALLOWED_HOSTS)?,
        shutdown_timeout: layered.get_duration(keys::KEY_HTTP_SHUTDOWN_TIMEOUT)?,
        shutdown_wait: layered.get_duration(keys::KEY_HTTP_SHUTDOWN_WAIT)?,
    })
}

/// Go `getRouterHealthConfig`.
fn get_router_health_config(
    layered: &Layered,
    halflife: Duration,
) -> crate::Result<RouterHealthConfig> {
    let config = RouterHealthConfig {
        max_drop_rate: layered.get_f64(keys::KEY_ROUTER_HEALTH_MAX_DROP_RATE)?,
        max_outstanding_requests: get_u32(
            layered,
            keys::KEY_ROUTER_HEALTH_MAX_OUTSTANDING_REQUESTS,
        )?,
        max_outstanding_duration: layered
            .get_duration(keys::KEY_NETWORK_HEALTH_MAX_OUTSTANDING_REQUEST_DURATION)?,
        max_run_time_requests: layered.get_duration(keys::KEY_NETWORK_MAXIMUM_TIMEOUT)?,
        max_drop_rate_halflife: halflife,
    };
    if !(0.0..=1.0).contains(&config.max_drop_rate) {
        return Err(invalid(
            keys::KEY_ROUTER_HEALTH_MAX_DROP_RATE,
            "must be in [0,1]",
        ));
    }
    if config.max_outstanding_duration.is_zero() {
        return Err(invalid(
            keys::KEY_NETWORK_HEALTH_MAX_OUTSTANDING_REQUEST_DURATION,
            "must be positive",
        ));
    }
    if config.max_run_time_requests.is_zero() {
        return Err(invalid(
            keys::KEY_NETWORK_MAXIMUM_TIMEOUT,
            "must be positive",
        ));
    }
    Ok(config)
}

/// Go `getAdaptiveTimeoutConfig`, in Go's validation branch order.
fn get_adaptive_timeout_config(layered: &Layered) -> crate::Result<AdaptiveTimeoutConfig> {
    let config = AdaptiveTimeoutConfig {
        initial_timeout: layered.get_duration(keys::KEY_NETWORK_INITIAL_TIMEOUT)?,
        minimum_timeout: layered.get_duration(keys::KEY_NETWORK_MINIMUM_TIMEOUT)?,
        maximum_timeout: layered.get_duration(keys::KEY_NETWORK_MAXIMUM_TIMEOUT)?,
        timeout_coefficient: layered.get_f64(keys::KEY_NETWORK_TIMEOUT_COEFFICIENT)?,
        timeout_halflife: layered.get_duration(keys::KEY_NETWORK_TIMEOUT_HALFLIFE)?,
    };
    if config.minimum_timeout.is_zero() {
        return Err(invalid(
            keys::KEY_NETWORK_MINIMUM_TIMEOUT,
            "must be positive",
        ));
    }
    if config.minimum_timeout > config.maximum_timeout {
        return Err(invalid(
            keys::KEY_NETWORK_MAXIMUM_TIMEOUT,
            format!("must be >= {:?}", keys::KEY_NETWORK_MINIMUM_TIMEOUT),
        ));
    }
    if config.initial_timeout < config.minimum_timeout
        || config.initial_timeout > config.maximum_timeout
    {
        return Err(invalid(
            keys::KEY_NETWORK_INITIAL_TIMEOUT,
            format!(
                "must be in [{:?}, {:?}]",
                keys::KEY_NETWORK_MINIMUM_TIMEOUT,
                keys::KEY_NETWORK_MAXIMUM_TIMEOUT
            ),
        ));
    }
    if config.timeout_halflife.is_zero() {
        return Err(invalid(keys::KEY_NETWORK_TIMEOUT_HALFLIFE, "must > 0"));
    }
    if config.timeout_coefficient < 1.0 {
        return Err(invalid(
            keys::KEY_NETWORK_TIMEOUT_COEFFICIENT,
            "must be >= 1",
        ));
    }
    Ok(config)
}

/// Go `getUpgradeConfig` — the embedded per-network schedule unless a custom
/// `--upgrade-file(-content)` is supplied on a non-standard network (13 §21).
/// The custom JSON is carried raw (the typed parse is the `ava-version` seam,
/// see [`Config::custom_upgrade_bytes`]).
fn get_upgrade_config(
    layered: &Layered,
    network_id: u32,
) -> crate::Result<(ava_version::upgrade::UpgradeConfig, Option<Vec<u8>>)> {
    let file_set = layered.is_set(keys::KEY_UPGRADE_FILE);
    let content_set = layered.is_set(keys::KEY_UPGRADE_FILE_CONTENT);
    if !file_set && !content_set {
        return Ok((ava_version::upgrade::get_config(network_id), None));
    }

    // TestnetID == FujiID.
    if matches!(network_id, MAINNET_ID | FUJI_ID | LOCAL_ID) {
        return Err(ConfigError::UpgradeNotAllowed {
            network: network_name(network_id),
        });
    }

    let upgrade_bytes = if file_set {
        read_file(&layered.get_expanded_string(keys::KEY_UPGRADE_FILE)?)?
    } else {
        decode_b64(&layered.get_string(keys::KEY_UPGRADE_FILE_CONTENT)?)?
    };
    // Go unmarshals into `upgrade.Config`; the typed parse is deferred, but
    // malformed JSON is still rejected here.
    serde_json::from_slice::<serde_json::Value>(&upgrade_bytes).map_err(|e| {
        ConfigError::Unmarshalling {
            what: "upgrade bytes".to_string(),
            msg: e.to_string(),
        }
    })?;
    Ok((
        ava_version::upgrade::get_config(network_id),
        Some(upgrade_bytes),
    ))
}

/// Parses an ACP int-slice flag into a `u32` set (each in `[0, i32::MAX]`).
fn get_acp_set(layered: &Layered, key: &str) -> crate::Result<BTreeSet<u32>> {
    let mut acps = BTreeSet::new();
    for acp in layered.get_int_slice(key)? {
        let acp = u32::try_from(acp)
            .ok()
            .filter(|v| *v <= u32::try_from(i32::MAX).unwrap_or(u32::MAX))
            .ok_or_else(|| invalid(key, format!("invalid ACP: {acp}")))?;
        acps.insert(acp);
    }
    Ok(acps)
}

/// Go `getNetworkConfig` (13 §8/§9), including the network-dependent
/// `--network-allow-private-ips` default and the ACP set algebra (13 §3).
fn get_network_config(
    layered: &Layered,
    network_id: u32,
    sybil_protection_enabled: bool,
    halflife: Duration,
) -> crate::Result<NetworkConfig> {
    // Max recent inbound connections upgraded = ceil(rate * cooldown secs).
    let max_inbound_conns_per_sec =
        layered.get_f64(keys::KEY_NETWORK_INBOUND_CONNECTION_THROTTLING_MAX_CONNS_PER_SEC)?;
    let inbound_connection_upgrade_cooldown =
        layered.get_duration(keys::KEY_NETWORK_INBOUND_CONNECTION_THROTTLING_COOLDOWN)?;
    let ceil = (max_inbound_conns_per_sec * inbound_connection_upgrade_cooldown.as_secs_f64())
        .ceil()
        .max(0.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let max_recent_conns_upgraded = ceil as u64;

    let compression_type = layered.get_string(keys::KEY_NETWORK_COMPRESSION_TYPE)?;
    if compression_type != "zstd" && compression_type != "none" {
        return Err(invalid(
            keys::KEY_NETWORK_COMPRESSION_TYPE,
            format!("unknown compression type: {compression_type:?}"),
        ));
    }

    // Network-dependent default: private IPs allowed off production networks
    // (`!constants.ProductionNetworkIDs.Contains(networkID)`; 13 §8).
    let mut allow_private_ips = !(network_id == MAINNET_ID || network_id == FUJI_ID);
    if layered.is_set(keys::KEY_NETWORK_ALLOW_PRIVATE_IPS) {
        allow_private_ips = layered.get_bool(keys::KEY_NETWORK_ALLOW_PRIVATE_IPS)?;
    }

    let mut supported_acps = get_acp_set(layered, keys::KEY_ACP_SUPPORT)?;
    let mut objected_acps = get_acp_set(layered, keys::KEY_ACP_OBJECT)?;
    if supported_acps.intersection(&objected_acps).next().is_some() {
        return Err(ConfigError::ConflictingACPOpinion);
    }
    if SCHEDULED_ACPS.iter().any(|acp| objected_acps.contains(acp)) {
        return Err(ConfigError::ConflictingImplicitACPOpinion);
    }
    // This node version has scheduled these ACPs: notify peers of support.
    supported_acps.extend(SCHEDULED_ACPS.iter().copied());
    // Peers are not notified of opinions on activated ACPs.
    for acp in ACTIVATED_ACPS {
        supported_acps.remove(acp);
        objected_acps.remove(acp);
    }

    let config = NetworkConfig {
        max_inbound_conns_per_sec,
        inbound_connection_upgrade_cooldown,
        max_recent_conns_upgraded,

        inbound_throttler_at_large_alloc_size: layered
            .get_u64(keys::KEY_THROTTLER_INBOUND_AT_LARGE_ALLOC_SIZE)?,
        inbound_throttler_vdr_alloc_size: layered
            .get_u64(keys::KEY_THROTTLER_INBOUND_VALIDATOR_ALLOC_SIZE)?,
        inbound_throttler_node_max_at_large_bytes: layered
            .get_u64(keys::KEY_THROTTLER_INBOUND_NODE_MAX_AT_LARGE_BYTES)?,
        inbound_throttler_bandwidth_refill_rate: layered
            .get_u64(keys::KEY_THROTTLER_INBOUND_BANDWIDTH_REFILL_RATE)?,
        inbound_throttler_bandwidth_max_burst_size: layered
            .get_u64(keys::KEY_THROTTLER_INBOUND_BANDWIDTH_MAX_BURST_SIZE)?,
        inbound_throttler_max_processing_msgs_per_node: layered
            .get_u64(keys::KEY_THROTTLER_INBOUND_NODE_MAX_PROCESSING_MSGS)?,
        inbound_throttler_cpu_max_recheck_delay: layered
            .get_duration(keys::KEY_THROTTLER_INBOUND_CPU_MAX_RECHECK_DELAY)?,
        inbound_throttler_disk_max_recheck_delay: layered
            .get_duration(keys::KEY_THROTTLER_INBOUND_DISK_MAX_RECHECK_DELAY)?,

        outbound_throttler_at_large_alloc_size: layered
            .get_u64(keys::KEY_THROTTLER_OUTBOUND_AT_LARGE_ALLOC_SIZE)?,
        outbound_throttler_vdr_alloc_size: layered
            .get_u64(keys::KEY_THROTTLER_OUTBOUND_VALIDATOR_ALLOC_SIZE)?,
        outbound_throttler_node_max_at_large_bytes: layered
            .get_u64(keys::KEY_THROTTLER_OUTBOUND_NODE_MAX_AT_LARGE_BYTES)?,

        health_enabled: sybil_protection_enabled,
        health_max_time_since_msg_sent: layered
            .get_duration(keys::KEY_NETWORK_HEALTH_MAX_TIME_SINCE_MSG_SENT)?,
        health_max_time_since_msg_received: layered
            .get_duration(keys::KEY_NETWORK_HEALTH_MAX_TIME_SINCE_MSG_RECEIVED)?,
        health_max_portion_send_queue_bytes_full: layered
            .get_f64(keys::KEY_NETWORK_HEALTH_MAX_PORTION_SEND_QUEUE_FULL)?,
        health_min_connected_peers: layered.get_u64(keys::KEY_NETWORK_HEALTH_MIN_CONN_PEERS)?,
        health_max_send_fail_rate: layered.get_f64(keys::KEY_NETWORK_HEALTH_MAX_SEND_FAIL_RATE)?,
        health_send_fail_rate_halflife: halflife,
        no_ingress_validator_connection_grace_period: layered
            .get_duration(keys::KEY_NETWORK_NO_INGRESS_CONNECTIONS_GRACE_PERIOD)?,

        proxy_enabled: layered.get_bool(keys::KEY_NETWORK_TCP_PROXY_ENABLED)?,
        proxy_read_header_timeout: layered
            .get_duration(keys::KEY_NETWORK_TCP_PROXY_READ_TIMEOUT)?,
        dialer_throttle_rps: get_u32(
            layered,
            keys::KEY_NETWORK_OUTBOUND_CONNECTION_THROTTLING_RPS,
        )?,
        dialer_connection_timeout: layered
            .get_duration(keys::KEY_NETWORK_OUTBOUND_CONNECTION_TIMEOUT)?,
        tls_key_log_file: layered.get_string(keys::KEY_NETWORK_TLS_KEY_LOG_FILE_UNSAFE)?,

        ping_pong_timeout: layered.get_duration(keys::KEY_NETWORK_PING_TIMEOUT)?,
        read_handshake_timeout: layered.get_duration(keys::KEY_NETWORK_READ_HANDSHAKE_TIMEOUT)?,
        peer_list_num_validator_ips: get_u32(
            layered,
            keys::KEY_NETWORK_PEER_LIST_NUM_VALIDATOR_IPS,
        )?,
        peer_list_pull_gossip_freq: layered
            .get_duration(keys::KEY_NETWORK_PEER_LIST_PULL_GOSSIP_FREQUENCY)?,
        peer_list_bloom_reset_freq: layered
            .get_duration(keys::KEY_NETWORK_PEER_LIST_BLOOM_RESET_FREQUENCY)?,
        max_reconnect_delay: layered.get_duration(keys::KEY_NETWORK_MAX_RECONNECT_DELAY)?,
        initial_reconnect_delay: layered.get_duration(keys::KEY_NETWORK_INITIAL_RECONNECT_DELAY)?,

        max_clock_difference: layered.get_duration(keys::KEY_NETWORK_MAX_CLOCK_DIFFERENCE)?,
        compression_type,
        ping_frequency: layered.get_duration(keys::KEY_NETWORK_PING_FREQUENCY)?,
        allow_private_ips,
        uptime_metric_freq: layered.get_duration(keys::KEY_UPTIME_METRIC_FREQ)?,
        maximum_inbound_message_timeout: layered
            .get_duration(keys::KEY_NETWORK_MAXIMUM_INBOUND_TIMEOUT)?,
        supported_acps,
        objected_acps,
        require_validator_to_connect: layered
            .get_bool(keys::KEY_NETWORK_REQUIRE_VALIDATOR_TO_CONNECT)?,
        peer_read_buffer_size: layered.get_u64(keys::KEY_NETWORK_PEER_READ_BUFFER_SIZE)?,
        peer_write_buffer_size: layered.get_u64(keys::KEY_NETWORK_PEER_WRITE_BUFFER_SIZE)?,
    };

    // Go's trailing switch; the `< 0` duration checks are vacuous here.
    if !(0.0..=1.0).contains(&config.health_max_send_fail_rate) {
        return Err(invalid(
            keys::KEY_NETWORK_HEALTH_MAX_SEND_FAIL_RATE,
            "must be in [0,1]",
        ));
    }
    if !(0.0..=1.0).contains(&config.health_max_portion_send_queue_bytes_full) {
        return Err(invalid(
            keys::KEY_NETWORK_HEALTH_MAX_PORTION_SEND_QUEUE_FULL,
            "must be in [0,1]",
        ));
    }
    if config.inbound_throttler_cpu_max_recheck_delay < MIN_INBOUND_THROTTLER_MAX_RECHECK_DELAY {
        return Err(invalid(
            keys::KEY_THROTTLER_INBOUND_CPU_MAX_RECHECK_DELAY,
            "must be >= 1ms",
        ));
    }
    if config.inbound_throttler_disk_max_recheck_delay < MIN_INBOUND_THROTTLER_MAX_RECHECK_DELAY {
        return Err(invalid(
            keys::KEY_THROTTLER_INBOUND_DISK_MAX_RECHECK_DELAY,
            "must be >= 1ms",
        ));
    }
    if config.max_reconnect_delay < config.initial_reconnect_delay {
        return Err(invalid(
            keys::KEY_NETWORK_MAX_RECONNECT_DELAY,
            format!("must be >= {}", keys::KEY_NETWORK_INITIAL_RECONNECT_DELAY),
        ));
    }
    if config.ping_pong_timeout <= config.ping_frequency {
        return Err(invalid(
            keys::KEY_NETWORK_PING_TIMEOUT,
            format!("must be > {}", keys::KEY_NETWORK_PING_FREQUENCY),
        ));
    }
    Ok(config)
}

/// Go `getBenchlistConfig` — `MaxPortion` derives from the primary network's
/// `alphaConfidence`/`k` (13 §10).
fn get_benchlist_config(
    layered: &Layered,
    snow: &SnowParameters,
) -> crate::Result<BenchlistConfig> {
    let alpha = f64::from(snow.alpha_confidence);
    let k = f64::from(snow.k);
    Ok(BenchlistConfig {
        halflife: layered.get_duration(keys::KEY_BENCHLIST_HALFLIFE)?,
        unbench_probability: layered.get_f64(keys::KEY_BENCHLIST_UNBENCH_PROBABILITY)?,
        bench_probability: layered.get_f64(keys::KEY_BENCHLIST_BENCH_PROBABILITY)?,
        bench_duration: layered.get_duration(keys::KEY_BENCHLIST_DURATION)?,
        max_portion: (1.0 - alpha / k) / 3.0,
    })
}

/// Go `getTxFeeConfig` — the fee flags apply only on non-standard networks;
/// Mainnet/Fuji use the genesis params (13 §4).
fn get_tx_fee_config(layered: &Layered, network_id: u32) -> crate::Result<TxFeeConfig> {
    if network_id != MAINNET_ID && network_id != FUJI_ID {
        return Ok(TxFeeConfig {
            create_asset_tx_fee: layered.get_u64(keys::KEY_CREATE_ASSET_TX_FEE)?,
            tx_fee: layered.get_u64(keys::KEY_TX_FEE)?,
            dynamic_fee_config: DynamicFeeConfig {
                weights: [
                    layered.get_u64(keys::KEY_DYNAMIC_FEES_BANDWIDTH_WEIGHT)?,
                    layered.get_u64(keys::KEY_DYNAMIC_FEES_DB_READ_WEIGHT)?,
                    layered.get_u64(keys::KEY_DYNAMIC_FEES_DB_WRITE_WEIGHT)?,
                    layered.get_u64(keys::KEY_DYNAMIC_FEES_COMPUTE_WEIGHT)?,
                ],
                max_capacity: layered.get_u64(keys::KEY_DYNAMIC_FEES_MAX_GAS_CAPACITY)?,
                max_per_second: layered.get_u64(keys::KEY_DYNAMIC_FEES_MAX_GAS_PER_SECOND)?,
                target_per_second: layered.get_u64(keys::KEY_DYNAMIC_FEES_TARGET_GAS_PER_SECOND)?,
                min_price: layered.get_u64(keys::KEY_DYNAMIC_FEES_MIN_GAS_PRICE)?,
                excess_conversion_constant: layered
                    .get_u64(keys::KEY_DYNAMIC_FEES_EXCESS_CONVERSION_CONSTANT)?,
            },
            validator_fee_config: ValidatorFeeConfig {
                capacity: layered.get_u64(keys::KEY_VALIDATOR_FEES_CAPACITY)?,
                target: layered.get_u64(keys::KEY_VALIDATOR_FEES_TARGET)?,
                min_price: layered.get_u64(keys::KEY_VALIDATOR_FEES_MIN_PRICE)?,
                excess_conversion_constant: layered
                    .get_u64(keys::KEY_VALIDATOR_FEES_EXCESS_CONVERSION_CONSTANT)?,
            },
        });
    }
    Ok(genesis_params::get_tx_fee_config(network_id))
}

/// Go `getGenesisData` — `--genesis-file-content`, then `--genesis-file`,
/// then the embedded config (13 §21).
fn get_genesis_data(
    layered: &Layered,
    network_id: u32,
    economics: &GenesisStakingConfig,
) -> crate::Result<(Vec<u8>, Id)> {
    if layered.is_set(keys::KEY_GENESIS_FILE_CONTENT) {
        let content = layered.get_string(keys::KEY_GENESIS_FILE_CONTENT)?;
        return Ok(ava_genesis::from_flag(
            network_id,
            &content,
            &economics.to_executor(),
        )?);
    }
    if layered.is_set(keys::KEY_GENESIS_FILE) {
        let path = layered.get_expanded_string(keys::KEY_GENESIS_FILE)?;
        return Ok(ava_genesis::from_file(
            network_id,
            Path::new(&path),
            &economics.to_executor(),
        )?);
    }
    Ok(ava_genesis::genesis_bytes(network_id, None)?)
}

/// Go `getStateSyncConfig` — ip/id lists must be the same length (13 §13).
fn get_state_sync_config(layered: &Layered) -> crate::Result<StateSyncConfig> {
    let mut config = StateSyncConfig::default();
    for ip in layered.get_string(keys::KEY_STATE_SYNC_IPS)?.split(',') {
        if ip.is_empty() {
            continue;
        }
        let addr: SocketAddr = ip.parse().map_err(|e| {
            invalid(
                keys::KEY_STATE_SYNC_IPS,
                format!("couldn't parse state sync ip {ip}: {e}"),
            )
        })?;
        config.state_sync_ips.push(addr);
    }
    for id in layered.get_string(keys::KEY_STATE_SYNC_IDS)?.split(',') {
        if id.is_empty() {
            continue;
        }
        let node_id = NodeId::from_str(id).map_err(|e| {
            invalid(
                keys::KEY_STATE_SYNC_IDS,
                format!("couldn't parse state sync peer id {id}: {e}"),
            )
        })?;
        config.state_sync_ids.push(node_id);
    }
    if config.state_sync_ips.len() != config.state_sync_ids.len() {
        return Err(ConfigError::StateSyncPeerCountMismatch {
            ips: config.state_sync_ips.len(),
            ids: config.state_sync_ids.len(),
        });
    }
    Ok(config)
}

/// Go `getBootstrapConfig` — both-or-neither `--bootstrap-ips`/`-ids`; when
/// neither is set the beacons are sampled from the genesis list (13 §13).
fn get_bootstrap_config(layered: &Layered, network_id: u32) -> crate::Result<BootstrapConfig> {
    let mut config = BootstrapConfig {
        bootstrappers: Vec::new(),
        bootstrap_beacon_connection_timeout: layered
            .get_duration(keys::KEY_BOOTSTRAP_BEACON_CONNECTION_TIMEOUT)?,
        bootstrap_max_time_get_ancestors: layered
            .get_duration(keys::KEY_BOOTSTRAP_MAX_TIME_GET_ANCESTORS)?,
        bootstrap_ancestors_max_containers_sent: get_u32(
            layered,
            keys::KEY_BOOTSTRAP_ANCESTORS_MAX_CONTAINERS_SENT,
        )?,
        bootstrap_ancestors_max_containers_received: get_u32(
            layered,
            keys::KEY_BOOTSTRAP_ANCESTORS_MAX_CONTAINERS_RECEIVED,
        )?,
    };

    let ips_set = layered.is_set(keys::KEY_BOOTSTRAP_IPS);
    let ids_set = layered.is_set(keys::KEY_BOOTSTRAP_IDS);
    if ips_set && !ids_set {
        return Err(ConfigError::BootstrapMutuallyRequired {
            set: keys::KEY_BOOTSTRAP_IPS.to_string(),
            unset: keys::KEY_BOOTSTRAP_IDS.to_string(),
        });
    }
    if !ips_set && ids_set {
        return Err(ConfigError::BootstrapMutuallyRequired {
            set: keys::KEY_BOOTSTRAP_IDS.to_string(),
            unset: keys::KEY_BOOTSTRAP_IPS.to_string(),
        });
    }
    if !ips_set && !ids_set {
        config.bootstrappers = ava_genesis::sample_bootstrappers(network_id, 5);
        return Ok(config);
    }

    let mut ips = Vec::new();
    for bootstrap_ip in layered.get_string(keys::KEY_BOOTSTRAP_IPS)?.split(',') {
        let ip = bootstrap_ip.trim();
        if ip.is_empty() {
            continue;
        }
        let addr: SocketAddr = ip.parse().map_err(|e| {
            invalid(
                keys::KEY_BOOTSTRAP_IPS,
                format!("couldn't parse bootstrap ip {ip}: {e}"),
            )
        })?;
        ips.push(addr);
    }

    let mut node_ids = Vec::new();
    for bootstrap_id in layered.get_string(keys::KEY_BOOTSTRAP_IDS)?.split(',') {
        let id = bootstrap_id.trim();
        if id.is_empty() {
            continue;
        }
        let node_id = NodeId::from_str(id).map_err(|e| {
            invalid(
                keys::KEY_BOOTSTRAP_IDS,
                format!("couldn't parse bootstrap peer id {id}: {e}"),
            )
        })?;
        node_ids.push(node_id);
    }

    if ips.len() != node_ids.len() {
        return Err(ConfigError::BootstrapPeerCountMismatch {
            ips: ips.len(),
            ids: node_ids.len(),
        });
    }
    config.bootstrappers = node_ids
        .into_iter()
        .zip(ips)
        .map(|(id, ip)| Bootstrapper { id, ip })
        .collect();
    Ok(config)
}

/// Go `getProfilerConfig`.
fn get_profiler_config(layered: &Layered) -> crate::Result<ProfilerConfig> {
    Ok(ProfilerConfig {
        dir: layered.get_expanded_string(keys::KEY_PROFILE_DIR)?,
        enabled: layered.get_bool(keys::KEY_PROFILE_CONTINUOUS_ENABLED)?,
        freq: layered.get_duration(keys::KEY_PROFILE_CONTINUOUS_FREQ)?,
        max_num_files: layered.get_i64(keys::KEY_PROFILE_CONTINUOUS_MAX_FILES)?,
    })
}

/// Go `getDiskSpaceConfig` — `(required, warning)`; warning must be within
/// `[0, 50]` and `>=` the required (fatal) threshold (13 §18).
fn get_disk_space_config(layered: &Layered) -> crate::Result<(u64, u64)> {
    let warn = layered.get_u64(keys::KEY_SYSTEM_TRACKER_DISK_WARNING_AVAILABLE_SPACE_PERCENTAGE)?;
    let required =
        layered.get_u64(keys::KEY_SYSTEM_TRACKER_DISK_REQUIRED_AVAILABLE_SPACE_PERCENTAGE)?;
    if warn > MAX_DISK_SPACE_THRESHOLD {
        return Err(ConfigError::DiskSpaceOutOfRange {
            key: keys::KEY_SYSTEM_TRACKER_DISK_WARNING_AVAILABLE_SPACE_PERCENTAGE.to_string(),
            value: warn,
        });
    }
    if warn < required {
        return Err(ConfigError::DiskWarnAfterFatal { warn, required });
    }
    Ok((required, warn))
}

/// Go `getCPUTargeterConfig` / `getDiskTargeterConfig` — all three allocs
/// must be non-negative.
fn get_targeter_config(
    layered: &Layered,
    vdr_key: &str,
    max_usage_key: &str,
    max_node_usage_key: &str,
) -> crate::Result<TargeterConfig> {
    let vdr_alloc = layered.get_f64(vdr_key)?;
    let max_non_vdr_usage = layered.get_f64(max_usage_key)?;
    let max_non_vdr_node_usage = layered.get_f64(max_node_usage_key)?;
    if vdr_alloc < 0.0 {
        return Err(invalid(vdr_key, format!("({vdr_alloc}) < 0")));
    }
    if max_non_vdr_usage < 0.0 {
        return Err(invalid(max_usage_key, format!("({max_non_vdr_usage}) < 0")));
    }
    if max_non_vdr_node_usage < 0.0 {
        return Err(invalid(
            max_node_usage_key,
            format!("({max_non_vdr_node_usage}) < 0"),
        ));
    }
    Ok(TargeterConfig {
        vdr_alloc,
        max_non_vdr_usage,
        max_non_vdr_node_usage,
    })
}

/// Go `getTraceConfig` (13 §22).
fn get_trace_config(layered: &Layered) -> crate::Result<TraceConfig> {
    let exporter_type_str = layered.get_string(keys::KEY_TRACING_EXPORTER_TYPE)?;
    let exporter_type = match exporter_type_str.to_lowercase().as_str() {
        "disabled" => TraceExporterType::Disabled,
        "grpc" => TraceExporterType::Grpc,
        "http" => TraceExporterType::Http,
        other => {
            return Err(invalid(
                keys::KEY_TRACING_EXPORTER_TYPE,
                format!("unknown exporter type: {other:?}"),
            ));
        }
    };
    Ok(TraceConfig {
        exporter_type,
        endpoint: layered.get_string(keys::KEY_TRACING_ENDPOINT)?,
        insecure: layered.get_bool(keys::KEY_TRACING_INSECURE)?,
        headers: layered.get_string_map(keys::KEY_TRACING_HEADERS)?,
        trace_sample_rate: layered.get_f64(keys::KEY_TRACING_SAMPLE_RATE)?,
        app_name: ava_version::application::CLIENT.to_string(),
        version: ava_version::CURRENT.to_string(),
    })
}

/// Go `providedFlags` — every key set at a non-default layer, with its
/// resolved value rendered to a string (13 §23).
fn provided_flags(layered: &Layered) -> crate::Result<BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for spec in FLAG_SPECS {
        if !layered.is_set(spec.key) {
            continue;
        }
        let value = match spec.kind {
            FlagKind::Bool => layered.get_bool(spec.key)?.to_string(),
            FlagKind::String => layered.get_string(spec.key)?,
            FlagKind::U64 | FlagKind::Uint => layered.get_u64(spec.key)?.to_string(),
            FlagKind::I64 => layered.get_i64(spec.key)?.to_string(),
            FlagKind::F64 => layered.get_f64(spec.key)?.to_string(),
            FlagKind::Duration => {
                crate::duration::format_go_duration(layered.get_duration(spec.key)?)
            }
            FlagKind::StringSlice => layered.get_string_slice(spec.key)?.join(","),
            FlagKind::IntSlice => layered
                .get_int_slice(spec.key)?
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(","),
            FlagKind::StringMap => {
                let map: BTreeMap<String, String> =
                    layered.get_string_map(spec.key)?.into_iter().collect();
                map.iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(",")
            }
        };
        out.insert(spec.key.to_string(), value);
    }
    Ok(out)
}

/// Go `config.GetNodeConfig` — the order-sensitive resolution of the layered
/// flags into the node [`Config`] (12 §1.6).
///
/// # Errors
///
/// Every sentinel in [`ConfigError`]'s `get_node_config` block, plus the
/// flag-read failures of the [`Layered`] getters.
pub fn get_node_config(layered: &Layered) -> crate::Result<Config> {
    let plugin_dir = get_plugin_dir(layered)?;

    let consensus_shutdown_timeout = layered.get_duration(keys::KEY_CONSENSUS_SHUTDOWN_TIMEOUT)?;

    // Gossiping.
    let frontier_poll_frequency =
        layered.get_duration(keys::KEY_CONSENSUS_FRONTIER_POLL_FREQUENCY)?;

    // App handling.
    let consensus_app_concurrency = get_u32(layered, keys::KEY_CONSENSUS_APP_CONCURRENCY)?;
    if consensus_app_concurrency == 0 {
        return Err(invalid(keys::KEY_CONSENSUS_APP_CONCURRENCY, "must be > 0"));
    }

    let use_current_height = layered.get_bool(keys::KEY_PROPOSERVM_USE_CURRENT_HEIGHT)?;

    // Logging.
    let logging_config = get_logging_config(layered)?;

    // Network ID.
    let network_id = parse_network_id(&layered.get_string(keys::KEY_NETWORK_ID)?)?;

    // Database.
    let database_config = get_database_config(layered, network_id)?;

    // IP configuration.
    let ip_config = get_ip_config(layered)?;

    // Staking.
    let staking_config = get_staking_config(layered, network_id)?;

    // Tracked subnets.
    let tracked_subnets = get_tracked_subnets(layered)?;

    // HTTP APIs.
    let http_config = get_http_config(layered)?;

    // Health (the freq `>= 0` check is vacuous; the halflife must be positive).
    let health_check_freq = layered.get_duration(keys::KEY_HEALTH_CHECK_FREQUENCY)?;
    let health_check_averager_halflife =
        layered.get_duration(keys::KEY_HEALTH_CHECK_AVERAGER_HALFLIFE)?;
    if health_check_averager_halflife.is_zero() {
        return Err(invalid(
            keys::KEY_HEALTH_CHECK_AVERAGER_HALFLIFE,
            "must be positive",
        ));
    }

    // Router.
    let router_health_config = get_router_health_config(layered, health_check_averager_halflife)?;

    // Metrics.
    let meter_vm_enabled = layered.get_bool(keys::KEY_METER_VMS_ENABLED)?;

    // Adaptive timeouts.
    let adaptive_timeout_config = get_adaptive_timeout_config(layered)?;

    // Upgrade schedule.
    let (upgrade_config, custom_upgrade_bytes) = get_upgrade_config(layered, network_id)?;

    // Networking.
    let network_config = get_network_config(
        layered,
        network_id,
        staking_config.sybil_protection_enabled,
        health_check_averager_halflife,
    )?;

    // Subnet configs (tracked subnets first, then the Primary Network entry).
    let subnet_ids: Vec<Id> = tracked_subnets.iter().copied().collect();
    let mut subnet_configs = subnets::get_subnet_configs(layered, &subnet_ids)?;
    let primary_network_config = subnets::primary_network_config(layered)?;
    primary_network_config.valid_parameters()?;
    let primary_snow = primary_network_config
        .snow_parameters
        .clone()
        .ok_or_else(|| ConfigError::InvalidConsensusParameters {
            msg: "primary network snow parameters must be set".to_string(),
        })?;
    subnet_configs.insert(PRIMARY_NETWORK_ID, primary_network_config);

    let proposer_min_block_delay = layered.get_duration(keys::KEY_PROPOSERVM_MIN_BLOCK_DELAY)?;

    // Benchlist.
    let benchlist_config = get_benchlist_config(layered, &primary_snow)?;

    // File descriptor limit.
    let fd_limit = layered.get_u64(keys::KEY_FD_LIMIT)?;

    // Tx fees.
    let tx_fee_config = get_tx_fee_config(layered, network_id)?;

    // Genesis data.
    let (genesis_bytes, avax_asset_id) =
        get_genesis_data(layered, network_id, &staking_config.economics)?;

    // State sync.
    let state_sync_config = get_state_sync_config(layered)?;

    // Bootstrap.
    let bootstrap_config = get_bootstrap_config(layered, network_id)?;

    // Chain configs.
    let chain_configs = chain_config::get_chain_configs(layered)?;

    // Profiler.
    let profiler_config = get_profiler_config(layered)?;

    // VM + chain aliases.
    let vm_aliases = chain_config::get_vm_aliases(layered)?;
    let chain_aliases = chain_config::get_chain_aliases(layered)?;

    // System tracker.
    let system_tracker_frequency = layered.get_duration(keys::KEY_SYSTEM_TRACKER_FREQUENCY)?;
    let system_tracker_processing_halflife =
        layered.get_duration(keys::KEY_SYSTEM_TRACKER_PROCESSING_HALFLIFE)?;
    let system_tracker_cpu_halflife =
        layered.get_duration(keys::KEY_SYSTEM_TRACKER_CPU_HALFLIFE)?;
    let system_tracker_disk_halflife =
        layered.get_duration(keys::KEY_SYSTEM_TRACKER_DISK_HALFLIFE)?;

    let (required_available_disk_space_percentage, warning_available_disk_space_percentage) =
        get_disk_space_config(layered)?;

    let cpu_targeter_config = get_targeter_config(
        layered,
        keys::KEY_THROTTLER_INBOUND_CPU_VALIDATOR_ALLOC,
        keys::KEY_THROTTLER_INBOUND_CPU_MAX_NON_VALIDATOR_USAGE,
        keys::KEY_THROTTLER_INBOUND_CPU_MAX_NON_VALIDATOR_NODE_USAGE,
    )?;
    let disk_targeter_config = get_targeter_config(
        layered,
        keys::KEY_THROTTLER_INBOUND_DISK_VALIDATOR_ALLOC,
        keys::KEY_THROTTLER_INBOUND_DISK_MAX_NON_VALIDATOR_USAGE,
        keys::KEY_THROTTLER_INBOUND_DISK_MAX_NON_VALIDATOR_NODE_USAGE,
    )?;

    // Tracing.
    let trace_config = get_trace_config(layered)?;

    let chain_data_dir = layered.get_expanded_string(keys::KEY_CHAIN_DATA_DIR)?;
    let process_context_file_path = layered.get_expanded_string(keys::KEY_PROCESS_CONTEXT_FILE)?;

    let provided_flags = provided_flags(layered)?;

    Ok(Config {
        plugin_dir,
        consensus_shutdown_timeout,
        frontier_poll_frequency,
        consensus_app_concurrency,
        use_current_height,
        logging_config,
        network_id,
        database_config,
        ip_config,
        staking_config,
        tracked_subnets,
        http_config,
        health_check_freq,
        router_health_config,
        meter_vm_enabled,
        adaptive_timeout_config,
        upgrade_config,
        custom_upgrade_bytes,
        network_config,
        subnet_configs,
        proposer_min_block_delay,
        benchlist_config,
        fd_limit,
        tx_fee_config,
        genesis_bytes,
        avax_asset_id,
        state_sync_config,
        bootstrap_config,
        chain_configs,
        profiler_config,
        vm_aliases,
        chain_aliases,
        system_tracker_frequency,
        system_tracker_processing_halflife,
        system_tracker_cpu_halflife,
        system_tracker_disk_halflife,
        required_available_disk_space_percentage,
        warning_available_disk_space_percentage,
        cpu_targeter_config,
        disk_targeter_config,
        trace_config,
        chain_data_dir,
        process_context_file_path,
        provided_flags,
    })
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;

    use super::*;
    use crate::ConfigError;
    use crate::flags::{FLAG_SPECS, build_command};
    use crate::precedence::Layered;
    use crate::subnets::PRIMARY_NETWORK_ID;

    /// Builds a `Layered` over a fresh tempdir data dir (so the plugin-dir /
    /// staking-cert side effects stay sandboxed) with an ephemeral staking
    /// cert (no disk keygen) and runs `get_node_config`.
    fn node_config(args: &[&str]) -> (crate::Result<crate::node::Config>, tempfile::TempDir) {
        let data = tempfile::tempdir().expect("tempdir");
        let mut all = vec![
            "avalanchers".to_string(),
            format!("--data-dir={}", data.path().display()),
            "--staking-ephemeral-cert-enabled=true".to_string(),
        ];
        all.extend(args.iter().map(ToString::to_string));
        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            all,
            FLAG_SPECS,
            std::iter::empty(),
        )
        .expect("layered");
        (get_node_config(&layered), data)
    }

    #[test]
    fn network_allow_private_ips_dependence() {
        // Unset: false for Mainnet/Fuji (production networks), true otherwise
        // (Go getNetworkConfig: !ProductionNetworkIDs.Contains; 13 §8).
        // Set: honored verbatim.
        let cases: [(&str, Option<bool>, bool); 6] = [
            ("mainnet", None, false),
            ("fuji", None, false),
            ("local", None, true),
            ("1337", None, true),
            ("mainnet", Some(true), true),
            ("local", Some(false), false),
        ];
        for (network, set, want) in cases {
            let mut args = vec![format!("--network-id={network}")];
            if let Some(v) = set {
                args.push(format!("--network-allow-private-ips={v}"));
            }
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            let (config, _dir) = node_config(&args);
            let config = config.unwrap_or_else(|e| panic!("{network}/{set:?}: {e}"));
            assert_eq!(
                config.network_config.allow_private_ips, want,
                "{network}/{set:?}"
            );
        }
    }

    #[test]
    fn sybil_protection_disabled_rejected_on_mainnet() {
        // Go errSybilProtectionDisabledOnPublicNetwork (13 §5).
        for network in ["mainnet", "fuji"] {
            let (config, _dir) = node_config(&[
                &format!("--network-id={network}"),
                "--sybil-protection-enabled=false",
            ]);
            assert_matches!(
                config,
                Err(ConfigError::SybilProtectionDisabledOnPublicNetwork),
                "{network}"
            );
        }

        // Allowed on local; recorded in the staking config.
        let (config, _dir) =
            node_config(&["--network-id=local", "--sybil-protection-enabled=false"]);
        let config = config.expect("local");
        assert!(!config.staking_config.sybil_protection_enabled);

        // Disabled weight must be positive when sybil protection is off.
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--sybil-protection-enabled=false",
            "--sybil-protection-disabled-weight=0",
        ]);
        assert_matches!(
            config,
            Err(ConfigError::SybilProtectionDisabledStakerWeights)
        );
    }

    #[test]
    fn bootstrappers_filled_from_genesis_when_unset() {
        // Both unset + standard network => genesis.SampleBootstrappers(net, 5)
        // (13 §13).
        let (config, _dir) = node_config(&["--network-id=fuji"]);
        let config = config.expect("fuji");
        let beacons = ava_genesis::bootstrappers(ava_types::constants::FUJI_ID);
        assert_eq!(
            config.bootstrap_config.bootstrappers.len(),
            5.min(beacons.len())
        );
        for b in &config.bootstrap_config.bootstrappers {
            assert!(
                beacons.contains(b),
                "sampled bootstrapper not in genesis list"
            );
        }

        // Both unset + custom network => empty.
        let (config, _dir) = node_config(&["--network-id=1337"]);
        assert!(
            config
                .expect("custom")
                .bootstrap_config
                .bootstrappers
                .is_empty()
        );

        // Mutually required: one set without the other errors.
        let (config, _dir) = node_config(&["--network-id=local", "--bootstrap-ips=127.0.0.1:9651"]);
        assert_matches!(config, Err(ConfigError::BootstrapMutuallyRequired { .. }));
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--bootstrap-ids=NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg",
        ]);
        assert_matches!(config, Err(ConfigError::BootstrapMutuallyRequired { .. }));

        // Mismatched counts error.
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--bootstrap-ips=127.0.0.1:9651,127.0.0.2:9651",
            "--bootstrap-ids=NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg",
        ]);
        assert_matches!(
            config,
            Err(ConfigError::BootstrapPeerCountMismatch { ips: 2, ids: 1 })
        );

        // Matching counts are zipped.
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--bootstrap-ips=127.0.0.1:9651",
            "--bootstrap-ids=NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg",
        ]);
        let config = config.expect("local");
        assert_eq!(config.bootstrap_config.bootstrappers.len(), 1);
        assert_eq!(
            config
                .bootstrap_config
                .bootstrappers
                .first()
                .expect("one bootstrapper")
                .ip,
            "127.0.0.1:9651".parse().expect("addr")
        );
    }

    #[test]
    fn snow_quorum_overrides_alpha() {
        // --snow-quorum-size overrides BOTH alphaPreference and
        // alphaConfidence; the dedicated flags are ignored (13 §7).
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--snow-quorum-size=18",
            "--snow-preference-quorum-size=14",
            "--snow-confidence-quorum-size=16",
        ]);
        let config = config.expect("local");
        let primary = config
            .subnet_configs
            .get(&PRIMARY_NETWORK_ID)
            .expect("primary network subnet config");
        let snow = primary.snow_parameters.as_ref().expect("snow params");
        assert_eq!(snow.alpha_preference, 18);
        assert_eq!(snow.alpha_confidence, 18);

        // Without it, the dedicated flags are honored.
        let (config, _dir) =
            node_config(&["--network-id=local", "--snow-preference-quorum-size=14"]);
        let config = config.expect("local");
        let snow = config
            .subnet_configs
            .get(&PRIMARY_NETWORK_ID)
            .expect("primary")
            .snow_parameters
            .clone()
            .expect("snow params");
        assert_eq!(snow.alpha_preference, 14);
        assert_eq!(snow.alpha_confidence, 15); // default

        // The benchlist MaxPortion derives from the primary alpha/k.
        let max_portion = config.benchlist_config.max_portion;
        assert!((max_portion - (1.0 - 15.0 / 20.0) / 3.0).abs() < 1e-12);
    }

    #[test]
    fn staking_economics_and_fees_ignored_on_standard_networks() {
        // 13 §4/§5: the fee + staking-economics flags only apply to
        // non-standard networks; Mainnet/Fuji use the genesis params.
        let (config, _dir) = node_config(&[
            "--network-id=fuji",
            "--tx-fee=123",
            "--uptime-requirement=0.5",
        ]);
        let config = config.expect("fuji");
        assert_eq!(config.tx_fee_config.tx_fee, 1_000_000);
        assert!((config.staking_config.economics.uptime_requirement - 0.8).abs() < 1e-12);

        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--tx-fee=123",
            "--uptime-requirement=0.5",
        ]);
        let config = config.expect("local");
        assert_eq!(config.tx_fee_config.tx_fee, 123);
        assert!((config.staking_config.economics.uptime_requirement - 0.5).abs() < 1e-12);

        // Genesis data resolved alongside (embedded for standard networks).
        assert!(!config.genesis_bytes.is_empty());
        assert_ne!(config.avax_asset_id, ava_types::id::Id::EMPTY);
    }

    #[test]
    fn one_of_validations() {
        // Staking signer: at most one option (Go errInvalidSignerConfig).
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--staking-ephemeral-signer-enabled=true",
            "--staking-rpc-signer-endpoint=http://signer",
        ]);
        assert_matches!(config, Err(ConfigError::InvalidSignerConfig));

        // public-ip XOR public-ip-resolution-service (13 §19).
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--public-ip=1.2.3.4",
            "--public-ip-resolution-service=opendns",
        ]);
        assert_matches!(config, Err(ConfigError::ConflictingPublicIpOptions));

        // Disk space percentages: warn <= 50, warn >= required (13 §18).
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--system-tracker-disk-warning-available-space-percentage=60",
        ]);
        assert_matches!(config, Err(ConfigError::DiskSpaceOutOfRange { .. }));
        let (config, _dir) = node_config(&[
            "--network-id=local",
            "--system-tracker-disk-warning-available-space-percentage=5",
            "--system-tracker-disk-required-available-space-percentage=10",
        ]);
        assert_matches!(config, Err(ConfigError::DiskWarnAfterFatal { .. }));

        // track-subnets must not contain the Primary Network (13 §14).
        let (config, _dir) = node_config(&[
            "--network-id=local",
            &format!("--track-subnets={PRIMARY_NETWORK_ID}"),
        ]);
        assert_matches!(config, Err(ConfigError::CannotTrackPrimaryNetwork));
    }
}
