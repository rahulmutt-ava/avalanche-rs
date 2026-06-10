// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The viper-parity layered precedence resolver (specs 12 §1.5, 13 §0/§23).
//!
//! `explicit CLI flag > env var (AVAGO_*) > config file > built-in default`.
//! This module owns the env-var snapshot (`AVAGO_NETWORK_ID` → `network-id`)
//! and the config-file loader (`--config-file-content` base64 wins over
//! `--config-file`; json/yaml/toml all funnel into one `serde_json::Value`).

use std::collections::HashMap;
use std::path::Path;

use base64::Engine as _;
use clap::ArgMatches;
use clap::parser::ValueSource;
use serde_json::Value;

use crate::ConfigError;
use crate::keys;

/// The env-var prefix (Go `config/viper.go::EnvPrefix`, upper-cased by
/// [`env_var_name`]).
pub const ENV_PREFIX: &str = "avago";

/// Go `config/viper.go::EnvVarName`: `AVAGO_` + `UPPER(key with '-'→'_')`,
/// e.g. `network-id` → `AVAGO_NETWORK_ID`.
#[must_use]
pub fn env_var_name(key: &str) -> String {
    format!("{ENV_PREFIX}_{key}")
        .replace('-', "_")
        .to_ascii_uppercase()
}

/// Builds the env layer from an explicit `(name, value)` iterator: keeps only
/// `AVAGO_*` vars, keyed by flag form (strip prefix, lowercase, `_`→`-`).
pub fn env_snapshot_from(vars: impl Iterator<Item = (String, String)>) -> HashMap<String, String> {
    vars.filter_map(|(name, value)| {
        name.strip_prefix("AVAGO_")
            .map(|rest| (rest.to_ascii_lowercase().replace('_', "-"), value))
    })
    .collect()
}

/// Snapshots the process environment once (viper `AutomaticEnv` +
/// `SetEnvPrefix("avago")` + `SetEnvKeyReplacer("-"→"_")`, 13 §0).
#[must_use]
pub fn env_snapshot() -> HashMap<String, String> {
    env_snapshot_from(std::env::vars())
}

/// Reads `key` from the explicit CLI layer, else the env layer (the two
/// layers that can name the config file itself; viper binds both).
fn cli_or_env(cli: &ArgMatches, env: &HashMap<String, String>, key: &str) -> Option<String> {
    if matches!(cli.value_source(key), Some(ValueSource::CommandLine)) {
        return cli.get_one::<String>(key).cloned();
    }
    env.get(key).cloned()
}

/// Loads the node config file into one `serde_json::Value` tree
/// (Go `config/viper.go::BuildViper`, 12 §1.5, 13 §0):
///
/// - `--config-file-content` (base64; parsed per `--config-file-content-type`
///   ∈ {json, yaml, toml}, default json) **overrides** `--config-file`;
/// - else `--config-file <path>` is read with the format inferred from its
///   extension;
/// - neither set → `Value::Null` (no file layer).
///
/// # Errors
///
/// [`ConfigError::InvalidBase64Content`] on a bad base64 blob,
/// [`ConfigError::ConfigContentTypeNotSupported`] on an unknown content type
/// or file extension, [`ConfigError::ConfigFileRead`] on an unreadable path,
/// and [`ConfigError::ConfigParse`] on malformed content.
pub fn load_config_file(cli: &ArgMatches, env: &HashMap<String, String>) -> crate::Result<Value> {
    if let Some(b64) = cli_or_env(cli, env, keys::KEY_CONFIG_FILE_CONTENT) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .map_err(|e| ConfigError::InvalidBase64Content { msg: e.to_string() })?;
        let content_type = cli_or_env(cli, env, keys::KEY_CONFIG_FILE_CONTENT_TYPE)
            .unwrap_or_else(|| "json".to_string());
        return parse_config_bytes(&bytes, &content_type);
    }
    if let Some(path) = cli_or_env(cli, env, keys::KEY_CONFIG_FILE) {
        // NOTE: getExpandedArg path expansion is applied by `Layered` (M8.10).
        let format = Path::new(&path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let format = if format == "yml" {
            "yaml".to_string()
        } else {
            format
        };
        let raw = std::fs::read(&path).map_err(|e| ConfigError::ConfigFileRead {
            path: path.clone(),
            msg: e.to_string(),
        })?;
        return parse_config_bytes(&raw, &format);
    }
    Ok(Value::Null)
}

/// Parses raw config bytes of the given type into a `serde_json::Value`.
fn parse_config_bytes(bytes: &[u8], content_type: &str) -> crate::Result<Value> {
    let parse_err = |e: &dyn std::fmt::Display| ConfigError::ConfigParse {
        format: content_type.to_string(),
        msg: e.to_string(),
    };
    match content_type {
        "json" => serde_json::from_slice(bytes).map_err(|e| parse_err(&e)),
        "yaml" => serde_yaml::from_slice(bytes).map_err(|e| parse_err(&e)),
        "toml" => {
            let text = std::str::from_utf8(bytes).map_err(|e| parse_err(&e))?;
            let value: toml::Value = toml::from_str(text).map_err(|e| parse_err(&e))?;
            serde_json::to_value(value).map_err(|e| parse_err(&e))
        }
        other => Err(ConfigError::ConfigContentTypeNotSupported {
            content_type: other.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_name_mapping() {
        // Go config/viper.go::EnvVarName — AVAGO_ + UPPER(key, '-'→'_').
        let cases = [
            ("network-id", "AVAGO_NETWORK_ID"),
            ("http-port", "AVAGO_HTTP_PORT"),
            (
                "network-tls-key-log-file-unsafe",
                "AVAGO_NETWORK_TLS_KEY_LOG_FILE_UNSAFE",
            ),
        ];
        for (key, var) in cases {
            assert_eq!(env_var_name(key), var, "{key}");
        }
        // And the inverse used by the snapshot: strip AVAGO_, lowercase, '_'→'-'.
        let env = env_snapshot_from(
            [
                ("AVAGO_NETWORK_ID".to_string(), "fuji".to_string()),
                ("AVAGO_HTTP_PORT".to_string(), "9750".to_string()),
                (
                    "AVAGO_NETWORK_TLS_KEY_LOG_FILE_UNSAFE".to_string(),
                    "/tmp/k".to_string(),
                ),
                ("HOME".to_string(), "/home/u".to_string()), // non-AVAGO ignored
            ]
            .into_iter(),
        );
        assert_eq!(env.get("network-id").map(String::as_str), Some("fuji"));
        assert_eq!(env.get("http-port").map(String::as_str), Some("9750"));
        assert_eq!(
            env.get("network-tls-key-log-file-unsafe")
                .map(String::as_str),
            Some("/tmp/k")
        );
        assert_eq!(env.len(), 3);
    }

    #[test]
    fn config_file_content_overrides_path() {
        use base64::Engine as _;

        use crate::flags::{FLAG_SPECS, build_command};

        // A config file on disk saying http-port=1111 …
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("conf.json");
        std::fs::write(&path, r#"{"http-port": 1111}"#).expect("write");

        // … loses to --config-file-content saying http-port=2222 (13 §0).
        let content = base64::engine::general_purpose::STANDARD.encode(r#"{"http-port": 2222}"#);
        let cli = build_command(FLAG_SPECS)
            .try_get_matches_from([
                "avalanchers".to_string(),
                format!("--config-file={}", path.display()),
                format!("--config-file-content={content}"),
            ])
            .expect("parse");
        let v = load_config_file(&cli, &std::collections::HashMap::new()).expect("load");
        assert_eq!(
            v.get("http-port").and_then(serde_json::Value::as_u64),
            Some(2222)
        );

        // Path alone is honored.
        let cli = build_command(FLAG_SPECS)
            .try_get_matches_from([
                "avalanchers".to_string(),
                format!("--config-file={}", path.display()),
            ])
            .expect("parse");
        let v = load_config_file(&cli, &std::collections::HashMap::new()).expect("load");
        assert_eq!(
            v.get("http-port").and_then(serde_json::Value::as_u64),
            Some(1111)
        );

        // Neither → Null.
        let cli = build_command(FLAG_SPECS)
            .try_get_matches_from(["avalanchers"])
            .expect("parse");
        let v = load_config_file(&cli, &std::collections::HashMap::new()).expect("load");
        assert!(v.is_null());
    }

    #[test]
    fn config_file_content_types_funnel_to_json_value() {
        use assert_matches::assert_matches;
        use base64::Engine as _;

        use crate::ConfigError;
        use crate::flags::{FLAG_SPECS, build_command};

        let std_b64 = base64::engine::general_purpose::STANDARD;
        let cases = [
            ("json", r#"{"network-id": "fuji", "http-port": 9750}"#),
            ("yaml", "network-id: fuji\nhttp-port: 9750\n"),
            ("toml", "network-id = \"fuji\"\nhttp-port = 9750\n"),
        ];
        for (content_type, body) in cases {
            let cli = build_command(FLAG_SPECS)
                .try_get_matches_from([
                    "avalanchers".to_string(),
                    format!("--config-file-content={}", std_b64.encode(body)),
                    format!("--config-file-content-type={content_type}"),
                ])
                .expect("parse");
            let v = load_config_file(&cli, &std::collections::HashMap::new())
                .unwrap_or_else(|e| panic!("{content_type}: {e}"));
            assert_eq!(
                v.get("network-id").and_then(serde_json::Value::as_str),
                Some("fuji"),
                "{content_type}"
            );
            assert_eq!(
                v.get("http-port").and_then(serde_json::Value::as_u64),
                Some(9750),
                "{content_type}"
            );
        }

        // Unsupported content type → the Go sentinel.
        let cli = build_command(FLAG_SPECS)
            .try_get_matches_from([
                "avalanchers".to_string(),
                format!("--config-file-content={}", std_b64.encode("{}")),
                "--config-file-content-type=xml".to_string(),
            ])
            .expect("parse");
        assert_matches!(
            load_config_file(&cli, &std::collections::HashMap::new()),
            Err(ConfigError::ConfigContentTypeNotSupported { .. })
        );

        // Bad base64 → decode error.
        let cli = build_command(FLAG_SPECS)
            .try_get_matches_from(["avalanchers", "--config-file-content=@@not-b64@@"])
            .expect("parse");
        assert_matches!(
            load_config_file(&cli, &std::collections::HashMap::new()),
            Err(ConfigError::InvalidBase64Content { .. })
        );
    }

    #[test]
    fn config_file_via_env_var() {
        // Go binds AVAGO_CONFIG_FILE via viper's AutomaticEnv: the env layer
        // can supply the config-file path when the CLI doesn't.
        use crate::flags::{FLAG_SPECS, build_command};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("conf.yaml");
        std::fs::write(&path, "http-port: 3333\n").expect("write");

        let cli = build_command(FLAG_SPECS)
            .try_get_matches_from(["avalanchers"])
            .expect("parse");
        let env = env_snapshot_from(
            [("AVAGO_CONFIG_FILE".to_string(), path.display().to_string())].into_iter(),
        );
        let v = load_config_file(&cli, &env).expect("load");
        assert_eq!(
            v.get("http-port").and_then(serde_json::Value::as_u64),
            Some(3333)
        );
    }
}
