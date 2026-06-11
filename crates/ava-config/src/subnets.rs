// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Per-subnet configuration + the subnet-config dir/content loaders
//! (Go `subnets/config.go` + `config/config.go::getSubnetConfigs`,
//! specs 12 §1.7, 13 §14).
//!
//! The partial-parameter types ([`SnowParameters`], [`SimplexParameters`])
//! mirror Go's serde shapes exactly (a zero field is "unset" and is filled
//! from the top-level flags by `applySubnetConfigDefaults`). The fully-filled
//! snow parameters convert into the owning crate's
//! `ava_snow::snowball::parameters::Parameters` (which has neither serde nor
//! the deprecated `alpha` field) via [`SnowParameters::to_snowball`] — that is
//! the seam between the config shape and the consensus type.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::Duration;

use ava_snow::snowball::parameters::Parameters as SnowballParameters;
use ava_types::id::Id;
use ava_types::node_id::NodeId;
use base64::Engine as _;
use serde::Deserialize;

use crate::error::ConfigError;
use crate::keys;
use crate::precedence::Layered;

/// The Primary Network's subnet ID (`constants.PrimaryNetworkID` == `ids.Empty`).
pub const PRIMARY_NETWORK_ID: Id = Id::EMPTY;

/// Per-subnet config file extension (Go `subnetConfigFileExt`).
const SUBNET_CONFIG_FILE_EXT: &str = ".json";

/// Deserializes a Go `time.Duration` JSON value: an integer is nanoseconds
/// (Go `encoding/json` on the underlying `int64`); a string additionally
/// accepts the Go duration grammar (a lenient superset for hand-written
/// configs).
fn de_duration_ns<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> core::result::Result<Duration, D::Error> {
    struct V;
    impl serde::de::Visitor<'_> for V {
        type Value = Duration;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("a duration (integer nanoseconds or a Go duration string)")
        }

        fn visit_u64<E: serde::de::Error>(self, v: u64) -> core::result::Result<Duration, E> {
            Ok(Duration::from_nanos(v))
        }

        fn visit_i64<E: serde::de::Error>(self, v: i64) -> core::result::Result<Duration, E> {
            u64::try_from(v)
                .map(Duration::from_nanos)
                .map_err(|_| E::custom("negative duration"))
        }

        fn visit_str<E: serde::de::Error>(self, v: &str) -> core::result::Result<Duration, E> {
            crate::duration::parse_go_duration(v).map_err(E::custom)
        }
    }
    d.deserialize_any(V)
}

/// Partial snowball parameters as they appear in a subnet-config JSON blob
/// (Go `snowball.Parameters` — including the deprecated `alpha` parse-only
/// field). A zero-valued field is "unset" (Go `applySnowballParameterDefaults`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SnowParameters {
    /// Nodes sampled per poll (`k`).
    pub k: u32,
    /// Deprecated parse-only alias: when present, sets both alphas.
    pub alpha: Option<u32>,
    /// Vote threshold to change preference (`alphaPreference`).
    pub alpha_preference: u32,
    /// Vote threshold to increase confidence (`alphaConfidence`).
    pub alpha_confidence: u32,
    /// Consecutive successful polls to finalize (`beta`).
    pub beta: u32,
    /// Target outstanding polls while processing (`concurrentRepolls`).
    pub concurrent_repolls: u32,
    /// Soft processing cap (`optimalProcessing`).
    pub optimal_processing: u32,
    /// Health cap on outstanding items (`maxOutstandingItems`).
    pub max_outstanding_items: u32,
    /// Health cap on per-item processing time (`maxItemProcessingTime`,
    /// integer nanoseconds in JSON like Go's `time.Duration`).
    #[serde(deserialize_with = "de_duration_ns")]
    pub max_item_processing_time: Duration,
}

impl SnowParameters {
    /// Converts the (fully-filled) config shape into the consensus crate's
    /// parameter type (`ava-snow`). The deprecated `alpha` field has already
    /// been folded into both alphas by the defaulting pass.
    #[must_use]
    pub fn to_snowball(&self) -> SnowballParameters {
        SnowballParameters {
            k: self.k,
            alpha_preference: self.alpha_preference,
            alpha_confidence: self.alpha_confidence,
            beta: self.beta,
            concurrent_repolls: self.concurrent_repolls,
            optimal_processing: self.optimal_processing,
            max_outstanding_items: self.max_outstanding_items,
            max_item_processing_time: self.max_item_processing_time,
        }
    }
}

/// Partial simplex parameters as they appear in a subnet-config JSON blob
/// (Go `simplex.Parameters`). Zero durations are "unset"
/// (Go `applySimplexDefaults`).
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct SimplexParameters {
    /// Upper bound on network message delay (`maxNetworkDelay`).
    #[serde(deserialize_with = "de_duration_ns")]
    pub max_network_delay: Duration,
    /// Upper bound on the rebroadcast wait (`maxRebroadcastWait`).
    #[serde(deserialize_with = "de_duration_ns")]
    pub max_rebroadcast_wait: Duration,
    /// The initial validator membership set. Kept as opaque JSON values:
    /// `ava_simplex::parameters::ValidatorInfo` has no serde impl, so the
    /// typed conversion happens at node-assembly time (documented seam,
    /// 12 §1.7).
    pub initial_validators: Vec<serde_json::Value>,
}

impl SimplexParameters {
    /// Go `simplex.Parameters.Verify`, in branch order.
    fn verify(&self) -> crate::Result<()> {
        if self.max_network_delay.is_zero() {
            return Err(ConfigError::InvalidConsensusParameters {
                msg: "maxNetworkDelay must be positive".to_string(),
            });
        }
        if self.max_rebroadcast_wait.is_zero() {
            return Err(ConfigError::InvalidConsensusParameters {
                msg: "maxRebroadcastWait must be positive".to_string(),
            });
        }
        if self.initial_validators.is_empty() {
            return Err(ConfigError::InvalidConsensusParameters {
                msg: "initialValidators must be non-empty".to_string(),
            });
        }
        Ok(())
    }
}

/// Per-subnet configuration (Go `subnets.Config`, 13 §14). After
/// [`subnet_config_from_bytes`] resolves the consensus mode, exactly one of
/// `snow_parameters`/`simplex_parameters` is `Some` and `consensus_parameters`
/// is always `None` (the deprecated form is migrated into `snow_parameters`).
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// This subnet's chains only talk to subnet validators (`validatorOnly`).
    pub validator_only: bool,
    /// Non-validators explicitly allowed when `validator_only` (`allowedNodes`).
    pub allowed_nodes: BTreeSet<NodeId>,
    /// Deprecated alias of `snow_parameters` (`consensusParameters`).
    pub consensus_parameters: Option<SnowParameters>,
    /// Snowball consensus overrides (`snowParameters`).
    pub snow_parameters: Option<SnowParameters>,
    /// Simplex consensus overrides (`simplexParameters`).
    pub simplex_parameters: Option<SimplexParameters>,
    /// Historical snowman++ blocks indexed per chain; 0 = all
    /// (`proposerNumHistoricalBlocks`).
    pub proposer_num_historical_blocks: u64,
}

impl Config {
    /// Go `Config.ValidConsensusConfiguration` — at most one consensus
    /// parameter type may be set.
    ///
    /// # Errors
    /// [`ConfigError::TooManyConsensusParameters`].
    pub fn valid_consensus_configuration(&self) -> crate::Result<()> {
        let num_set = usize::from(self.simplex_parameters.is_some())
            .saturating_add(usize::from(self.snow_parameters.is_some()))
            .saturating_add(usize::from(self.consensus_parameters.is_some()));
        if num_set > 1 {
            return Err(ConfigError::TooManyConsensusParameters);
        }
        Ok(())
    }

    /// Go `Config.ValidParameters` — `allowedNodes` requires `validatorOnly`,
    /// then the resolved consensus parameters must `Verify()`.
    ///
    /// # Errors
    /// [`ConfigError::AllowedNodesWhenNotValidatorOnly`] /
    /// [`ConfigError::InvalidConsensusParameters`].
    pub fn valid_parameters(&self) -> crate::Result<()> {
        if !self.validator_only && !self.allowed_nodes.is_empty() {
            return Err(ConfigError::AllowedNodesWhenNotValidatorOnly);
        }
        if let Some(snow) = &self.snow_parameters {
            return snow
                .to_snowball()
                .verify()
                .map_err(|e| ConfigError::InvalidConsensusParameters { msg: e.to_string() });
        }
        if let Some(simplex) = &self.simplex_parameters {
            return simplex.verify();
        }
        Err(ConfigError::InvalidConsensusParameters {
            msg: "consensus config must have either snowball or simplex parameters set".to_string(),
        })
    }
}

/// Reads an `i64` flag as `u32` (Go `viper.GetInt` into an `int` snow field).
fn get_u32(layered: &Layered, key: &str) -> crate::Result<u32> {
    let v = layered.get_i64(key)?;
    u32::try_from(v).map_err(|_| ConfigError::InvalidValue {
        key: key.to_string(),
        msg: format!("({v}) must be in [0, {}]", u32::MAX),
    })
}

/// Go `getPrimaryNetworkSnowConfig` — the snow parameters from the top-level
/// flags, with the `--snow-quorum-size` override of BOTH alphas (13 §7).
///
/// # Errors
/// Propagates flag-read failures.
pub(crate) fn primary_network_snow_config(layered: &Layered) -> crate::Result<SnowParameters> {
    let mut p = SnowParameters {
        k: get_u32(layered, keys::KEY_SNOW_SAMPLE_SIZE)?,
        alpha: None,
        alpha_preference: get_u32(layered, keys::KEY_SNOW_PREFERENCE_QUORUM_SIZE)?,
        alpha_confidence: get_u32(layered, keys::KEY_SNOW_CONFIDENCE_QUORUM_SIZE)?,
        beta: get_u32(layered, keys::KEY_SNOW_COMMIT_THRESHOLD)?,
        concurrent_repolls: get_u32(layered, keys::KEY_SNOW_CONCURRENT_REPOLLS)?,
        optimal_processing: get_u32(layered, keys::KEY_SNOW_OPTIMAL_PROCESSING)?,
        max_outstanding_items: get_u32(layered, keys::KEY_SNOW_MAX_PROCESSING)?,
        max_item_processing_time: layered.get_duration(keys::KEY_SNOW_MAX_TIME_PROCESSING)?,
    };
    if layered.is_set(keys::KEY_SNOW_QUORUM_SIZE) {
        p.alpha_preference = get_u32(layered, keys::KEY_SNOW_QUORUM_SIZE)?;
        p.alpha_confidence = p.alpha_preference;
    }
    Ok(p)
}

/// Go `applySnowballParameterDefaults` — fills zero-valued fields from the
/// top-level flags; the deprecated `alpha` (if present) then overrides both
/// alphas.
fn apply_snowball_parameter_defaults(
    config: &mut SnowParameters,
    layered: &Layered,
) -> crate::Result<()> {
    if config.k == 0 {
        config.k = get_u32(layered, keys::KEY_SNOW_SAMPLE_SIZE)?;
    }
    if config.alpha_preference == 0 {
        config.alpha_preference = if layered.is_set(keys::KEY_SNOW_QUORUM_SIZE) {
            get_u32(layered, keys::KEY_SNOW_QUORUM_SIZE)?
        } else {
            get_u32(layered, keys::KEY_SNOW_PREFERENCE_QUORUM_SIZE)?
        };
    }
    if config.alpha_confidence == 0 {
        config.alpha_confidence = if layered.is_set(keys::KEY_SNOW_QUORUM_SIZE) {
            get_u32(layered, keys::KEY_SNOW_QUORUM_SIZE)?
        } else {
            get_u32(layered, keys::KEY_SNOW_CONFIDENCE_QUORUM_SIZE)?
        };
    }
    if config.beta == 0 {
        config.beta = get_u32(layered, keys::KEY_SNOW_COMMIT_THRESHOLD)?;
    }
    if config.concurrent_repolls == 0 {
        config.concurrent_repolls = get_u32(layered, keys::KEY_SNOW_CONCURRENT_REPOLLS)?;
    }
    if config.optimal_processing == 0 {
        config.optimal_processing = get_u32(layered, keys::KEY_SNOW_OPTIMAL_PROCESSING)?;
    }
    if config.max_outstanding_items == 0 {
        config.max_outstanding_items = get_u32(layered, keys::KEY_SNOW_MAX_PROCESSING)?;
    }
    if config.max_item_processing_time.is_zero() {
        config.max_item_processing_time =
            layered.get_duration(keys::KEY_SNOW_MAX_TIME_PROCESSING)?;
    }
    if let Some(alpha) = config.alpha {
        config.alpha_preference = alpha;
        config.alpha_confidence = alpha;
    }
    Ok(())
}

/// Go `applySimplexDefaults` — fills zero durations from the simplex flags.
fn apply_simplex_defaults(config: &mut SimplexParameters, layered: &Layered) -> crate::Result<()> {
    if config.max_network_delay.is_zero() {
        config.max_network_delay = layered.get_duration(keys::KEY_SIMPLEX_MAX_NETWORK_DELAY)?;
    }
    if config.max_rebroadcast_wait.is_zero() {
        config.max_rebroadcast_wait =
            layered.get_duration(keys::KEY_SIMPLEX_MAX_REBROADCAST_WAIT)?;
    }
    Ok(())
}

/// Go `applySubnetConfigDefaults` — resolves the consensus mode and fills
/// unset fields (migrating the deprecated `consensusParameters` into
/// `snowParameters`).
fn apply_subnet_config_defaults(config: &mut Config, layered: &Layered) -> crate::Result<()> {
    if let Some(simplex) = &mut config.simplex_parameters {
        return apply_simplex_defaults(simplex, layered);
    }
    if let Some(snow) = &mut config.snow_parameters {
        return apply_snowball_parameter_defaults(snow, layered);
    }
    if let Some(mut deprecated) = config.consensus_parameters.take() {
        apply_snowball_parameter_defaults(&mut deprecated, layered)?;
        config.snow_parameters = Some(deprecated);
        return Ok(());
    }
    config.snow_parameters = Some(primary_network_snow_config(layered)?);
    Ok(())
}

/// The serde overlay shape: only the fields present in the JSON replace the
/// primary-network base config (Go unmarshals *over* the base struct).
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawSubnetConfig {
    validator_only: Option<bool>,
    allowed_nodes: Option<BTreeSet<NodeId>>,
    consensus_parameters: Option<SnowParameters>,
    snow_parameters: Option<SnowParameters>,
    simplex_parameters: Option<SimplexParameters>,
    proposer_num_historical_blocks: Option<u64>,
}

/// Go `getSubnetConfigFromBytes` — unmarshal a subnet config over the
/// primary-network defaults (consensus pointers cleared), validate that at
/// most one consensus block is set, fill unset fields, and validate.
///
/// # Errors
/// [`ConfigError::Unmarshalling`] on bad JSON, plus everything
/// [`Config::valid_consensus_configuration`] / [`Config::valid_parameters`]
/// can return.
pub fn subnet_config_from_bytes(raw_bytes: &[u8], layered: &Layered) -> crate::Result<Config> {
    // Start from the primary-network config with the consensus params cleared.
    let mut config = primary_network_config(layered)?;
    config.snow_parameters = None;
    config.simplex_parameters = None;
    config.consensus_parameters = None;

    let raw: RawSubnetConfig =
        serde_json::from_slice(raw_bytes).map_err(|e| ConfigError::Unmarshalling {
            what: "subnet config".to_string(),
            msg: e.to_string(),
        })?;
    if let Some(v) = raw.validator_only {
        config.validator_only = v;
    }
    if let Some(v) = raw.allowed_nodes {
        config.allowed_nodes = v;
    }
    config.consensus_parameters = raw.consensus_parameters;
    config.snow_parameters = raw.snow_parameters;
    config.simplex_parameters = raw.simplex_parameters;
    if let Some(v) = raw.proposer_num_historical_blocks {
        config.proposer_num_historical_blocks = v;
    }

    config.valid_consensus_configuration()?;
    apply_subnet_config_defaults(&mut config, layered)?;
    config.valid_parameters()?;
    Ok(config)
}

/// Go `getPrimaryNetworkConfig` — `validatorOnly=false`, snow params from the
/// top-level flags, `proposerNumHistoricalBlocks=0`
/// (`proposervm.DefaultNumHistoricalBlocks`).
///
/// # Errors
/// Propagates flag-read failures.
pub fn primary_network_config(layered: &Layered) -> crate::Result<Config> {
    Ok(Config {
        validator_only: false,
        allowed_nodes: BTreeSet::new(),
        consensus_parameters: None,
        snow_parameters: Some(primary_network_snow_config(layered)?),
        simplex_parameters: None,
        proposer_num_historical_blocks: 0,
    })
}

/// Go `getSubnetConfigs` — per-subnet configs from `--subnet-config-content`
/// (b64 JSON map) when set, else `<subnet-config-dir>/<subnetID>.json` files;
/// subnets without an entry get the primary-network defaults.
///
/// # Errors
/// [`ConfigError::InvalidBase64Content`] / [`ConfigError::Unmarshalling`] /
/// [`ConfigError::CannotReadDirectory`] / the per-subnet parse failures.
pub fn get_subnet_configs(
    layered: &Layered,
    subnet_ids: &[Id],
) -> crate::Result<HashMap<Id, Config>> {
    if layered.is_set(keys::KEY_SUBNET_CONFIG_CONTENT) {
        return get_subnet_configs_from_flags(layered, subnet_ids);
    }
    get_subnet_configs_from_dir(layered, subnet_ids)
}

/// Go `getSubnetConfigsFromFlags`.
fn get_subnet_configs_from_flags(
    layered: &Layered,
    subnet_ids: &[Id],
) -> crate::Result<HashMap<Id, Config>> {
    let content_b64 = layered.get_string(keys::KEY_SUBNET_CONFIG_CONTENT)?;
    let content = base64::engine::general_purpose::STANDARD
        .decode(content_b64.trim())
        .map_err(|e| ConfigError::InvalidBase64Content { msg: e.to_string() })?;

    // Partially parse to raw values, to be filled by defaults later.
    let raw_configs: HashMap<Id, serde_json::Value> =
        serde_json::from_slice(&content).map_err(|e| ConfigError::Unmarshalling {
            what: "subnet configs".to_string(),
            msg: e.to_string(),
        })?;

    let mut res = HashMap::with_capacity(subnet_ids.len());
    for subnet_id in subnet_ids {
        let Some(raw) = raw_configs.get(subnet_id) else {
            res.insert(*subnet_id, primary_network_config(layered)?);
            continue;
        };
        let raw_bytes = serde_json::to_vec(raw).map_err(|e| ConfigError::Unmarshalling {
            what: "subnet configs".to_string(),
            msg: e.to_string(),
        })?;
        let config = subnet_config_from_bytes(&raw_bytes, layered)?;
        res.insert(*subnet_id, config);
    }
    Ok(res)
}

/// Go `getSubnetConfigsFromDir`.
fn get_subnet_configs_from_dir(
    layered: &Layered,
    subnet_ids: &[Id],
) -> crate::Result<HashMap<Id, Config>> {
    let subnet_config_path =
        crate::chain_config::path_from_dir_key(layered, keys::KEY_SUBNET_CONFIG_DIR)?;

    let mut subnet_configs = HashMap::with_capacity(subnet_ids.len());
    for subnet_id in subnet_ids {
        let Some(dir) = &subnet_config_path else {
            // Path does not exist but was not explicitly specified: defaults.
            subnet_configs.insert(*subnet_id, primary_network_config(layered)?);
            continue;
        };
        let file_path = dir.join(format!("{subnet_id}{SUBNET_CONFIG_FILE_EXT}"));
        match std::fs::metadata(&file_path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // This subnet config does not exist: defaults.
                subnet_configs.insert(*subnet_id, primary_network_config(layered)?);
                continue;
            }
            Err(e) => {
                return Err(read_err(&file_path, &e));
            }
            Ok(info) if info.is_dir() => {
                return Err(ConfigError::InvalidValue {
                    key: keys::KEY_SUBNET_CONFIG_DIR.to_string(),
                    msg: format!("{file_path:?} is a directory, expected a file"),
                });
            }
            Ok(_) => {}
        }
        let file = std::fs::read(&file_path).map_err(|e| read_err(&file_path, &e))?;
        let config = subnet_config_from_bytes(&file, layered)?;
        subnet_configs.insert(*subnet_id, config);
    }
    Ok(subnet_configs)
}

/// Maps a filesystem error on a subnet config file to a [`ConfigError`].
fn read_err(path: &Path, e: &std::io::Error) -> ConfigError {
    ConfigError::ConfigFileRead {
        path: path.display().to_string(),
        msg: e.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use assert_matches::assert_matches;
    use ava_types::id::Id;
    use base64::Engine as _;

    use super::*;
    use crate::ConfigError;
    use crate::flags::{FLAG_SPECS, build_command};
    use crate::precedence::Layered;

    fn layered(args: &[&str]) -> Layered {
        let mut all = vec!["avalanchers".to_string()];
        all.extend(args.iter().map(ToString::to_string));
        Layered::build_with_env(
            build_command(FLAG_SPECS),
            all,
            FLAG_SPECS,
            std::iter::empty(),
        )
        .expect("layered")
    }

    const NODE: &str = "NodeID-7Xhw2mDxuDS44j42TCB6U5579esbSt3Lg";

    #[test]
    fn resolve_consensus_mode() {
        let l = layered(&["--snow-sample-size=11", "--snow-quorum-size=9"]);

        // At most one of consensusParameters/snowParameters/simplexParameters
        // (Go ErrTooManyConsensusParameters, 13 §14).
        let two = br#"{"snowParameters":{"k":12},"simplexParameters":{"maxNetworkDelay":1}}"#;
        assert_matches!(
            subnet_config_from_bytes(two, &l),
            Err(ConfigError::TooManyConsensusParameters)
        );
        let dep_and_snow = br#"{"consensusParameters":{"k":12},"snowParameters":{"k":12}}"#;
        assert_matches!(
            subnet_config_from_bytes(dep_and_snow, &l),
            Err(ConfigError::TooManyConsensusParameters)
        );

        // None set -> the primary-network snow config from the top-level flags,
        // including the snow-quorum-size override of BOTH alphas (13 §7).
        let cfg = subnet_config_from_bytes(b"{}", &l).expect("default");
        let snow = cfg.snow_parameters.expect("snow");
        assert_eq!(snow.k, 11);
        assert_eq!(snow.alpha_preference, 9);
        assert_eq!(snow.alpha_confidence, 9);
        assert!(cfg.simplex_parameters.is_none());
        assert!(cfg.consensus_parameters.is_none());
        assert!(!cfg.validator_only);
        assert_eq!(cfg.proposer_num_historical_blocks, 0);

        // allowedNodes non-empty requires validatorOnly=true
        // (Go errAllowedNodesWhenNotValidatorOnly).
        let allowed = format!(r#"{{"allowedNodes":["{NODE}"]}}"#);
        assert_matches!(
            subnet_config_from_bytes(allowed.as_bytes(), &l),
            Err(ConfigError::AllowedNodesWhenNotValidatorOnly)
        );
        let ok = format!(r#"{{"validatorOnly":true,"allowedNodes":["{NODE}"]}}"#);
        let cfg = subnet_config_from_bytes(ok.as_bytes(), &l).expect("validator-only");
        assert!(cfg.validator_only);
        assert_eq!(cfg.allowed_nodes.len(), 1);

        // Deprecated consensusParameters migrates into snowParameters; unset
        // (zero) fields default from the flags; `alpha` sets both alphas.
        let dep = br#"{"consensusParameters":{"k":21,"alpha":16}}"#;
        let cfg = subnet_config_from_bytes(dep, &l).expect("deprecated");
        assert!(cfg.consensus_parameters.is_none());
        let snow = cfg.snow_parameters.expect("migrated");
        assert_eq!(snow.k, 21);
        assert_eq!(snow.alpha_preference, 16);
        assert_eq!(snow.alpha_confidence, 16);
        assert_eq!(snow.beta, 20); // --snow-commit-threshold default
        assert_eq!(snow.max_item_processing_time, Duration::from_secs(30));

        // simplexParameters: unset fields fill from the simplex flags; an empty
        // initialValidators set fails Verify (Go parity).
        let simplex = br#"{"simplexParameters":{"maxNetworkDelay":1000000000}}"#;
        assert_matches!(
            subnet_config_from_bytes(simplex, &l),
            Err(ConfigError::InvalidConsensusParameters { .. })
        );
        let simplex_ok = format!(
            r#"{{"simplexParameters":{{"maxNetworkDelay":1000000000,"initialValidators":[{{"nodeId":"{NODE}"}}]}}}}"#
        );
        let cfg = subnet_config_from_bytes(simplex_ok.as_bytes(), &l).expect("simplex");
        let sp = cfg.simplex_parameters.expect("simplex params");
        assert_eq!(sp.max_network_delay, Duration::from_secs(1));
        assert_eq!(sp.max_rebroadcast_wait, Duration::from_secs(5)); // flag default
        assert!(cfg.snow_parameters.is_none());

        // Invalid snow parameters surface the Verify failure.
        let bad = br#"{"snowParameters":{"k":2,"alphaPreference":1,"alphaConfidence":1}}"#;
        assert_matches!(
            subnet_config_from_bytes(bad, &l),
            Err(ConfigError::InvalidConsensusParameters { .. })
        );

        // Malformed JSON -> the unmarshalling sentinel.
        assert_matches!(
            subnet_config_from_bytes(b"not-json", &l),
            Err(ConfigError::Unmarshalling { .. })
        );
    }

    #[test]
    fn subnet_config_sources() {
        let tracked = Id::from([1u8; 32]);
        let untouched = Id::from([2u8; 32]);

        // Dir form: <subnet-config-dir>/<subnetID>.json; missing files default
        // to the primary-network config (13 §14).
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join(format!("{tracked}.json")),
            r#"{"validatorOnly":true}"#,
        )
        .expect("write");
        let l = layered(&[&format!("--subnet-config-dir={}", dir.path().display())]);
        let configs = get_subnet_configs(&l, &[tracked, untouched]).expect("configs");
        assert!(configs.get(&tracked).expect("tracked").validator_only);
        assert!(!configs.get(&untouched).expect("untouched").validator_only);
        assert!(
            configs
                .get(&untouched)
                .expect("untouched")
                .snow_parameters
                .is_some()
        );

        // Explicitly-set but missing dir -> errCannotReadDirectory.
        let l = layered(&["--subnet-config-dir=/definitely/not/a/dir"]);
        assert_matches!(
            get_subnet_configs(&l, &[tracked]),
            Err(ConfigError::CannotReadDirectory { .. })
        );

        // Unset + missing default dir -> every tracked subnet gets defaults.
        let data = tempfile::tempdir().expect("tempdir");
        let l = layered(&[&format!("--data-dir={}", data.path().display())]);
        let configs = get_subnet_configs(&l, &[tracked]).expect("configs");
        assert!(!configs.get(&tracked).expect("tracked").validator_only);

        // Content form (b64 JSON map) overrides the dir.
        let content = base64::engine::general_purpose::STANDARD
            .encode(format!(r#"{{"{tracked}":{{"validatorOnly":true}}}}"#));
        let l = layered(&[
            "--subnet-config-dir=/definitely/not/a/dir",
            &format!("--subnet-config-content={content}"),
        ]);
        let configs = get_subnet_configs(&l, &[tracked, untouched]).expect("configs");
        assert!(configs.get(&tracked).expect("tracked").validator_only);
        assert!(!configs.get(&untouched).expect("untouched").validator_only);
    }
}
