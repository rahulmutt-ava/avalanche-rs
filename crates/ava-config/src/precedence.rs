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

/// The Go env var that names the resolved data dir in path values.
pub const DATA_DIR_VAR: &str = "AVALANCHEGO_DATA_DIR";

/// The layered config resolver (viper shim, 12 §1.5):
/// `explicit CLI flag > env (AVAGO_*) > config file > built-in default`.
pub struct Layered {
    cli: ArgMatches,
    env: HashMap<String, String>,
    file: Value,
    specs: HashMap<&'static str, &'static crate::flags::FlagSpec>,
    /// Full OS-environment snapshot, for `$VAR` path expansion (os.Expand).
    os_env: HashMap<String, String>,
}

impl Layered {
    /// Builds the resolver from the live process environment.
    ///
    /// # Errors
    ///
    /// [`ConfigError::CliParse`] on a clap parse failure, plus everything
    /// [`load_config_file`] can return.
    pub fn build(
        cmd: clap::Command,
        args: impl IntoIterator<Item = String>,
        specs: &'static [crate::flags::FlagSpec],
    ) -> crate::Result<Self> {
        Self::build_with_env(cmd, args, specs, std::env::vars())
    }

    /// [`Self::build`] with an explicit environment snapshot (testable; the
    /// env layer AND `$VAR` expansion both read from `vars`).
    ///
    /// # Errors
    ///
    /// See [`Self::build`].
    pub fn build_with_env(
        cmd: clap::Command,
        args: impl IntoIterator<Item = String>,
        specs: &'static [crate::flags::FlagSpec],
        vars: impl Iterator<Item = (String, String)>,
    ) -> crate::Result<Self> {
        let cli = cmd
            .try_get_matches_from(args)
            .map_err(|e| ConfigError::CliParse { msg: e.to_string() })?;
        let os_env: HashMap<String, String> = vars.collect();
        let env = env_snapshot_from(os_env.iter().map(|(k, v)| (k.clone(), v.clone())));
        let mut layered = Self {
            cli,
            env,
            file: Value::Null,
            specs: specs.iter().map(|s| (s.key, s)).collect(),
            os_env,
        };
        // The config-file PATH itself is read through getExpandedArg before
        // the file layer exists (Go BuildViper order), so expansion sees only
        // CLI/env/default data-dir.
        if let Some(path) = cli_or_env(&layered.cli, &layered.env, keys::KEY_CONFIG_FILE) {
            let expanded = layered.expand(&path);
            if expanded != path {
                layered.file = load_config_file_at(&layered.cli, &layered.env, &expanded)?;
                return Ok(layered);
            }
        }
        layered.file = load_config_file(&layered.cli, &layered.env)?;
        Ok(layered)
    }

    /// The spec row for `key`, or [`ConfigError::UnknownKey`].
    fn spec(&self, key: &str) -> crate::Result<&'static crate::flags::FlagSpec> {
        self.specs
            .get(key)
            .copied()
            .ok_or_else(|| ConfigError::UnknownKey {
                key: key.to_string(),
            })
    }

    /// True if the key was provided at any non-default layer
    /// (viper `IsSet`, 13 §23).
    #[must_use]
    pub fn is_set(&self, key: &str) -> bool {
        matches!(self.cli.value_source(key), Some(ValueSource::CommandLine))
            || self.env.contains_key(key)
            || self.file_lookup(key).is_some()
    }

    /// Case-insensitive, `_`→`-`-folded lookup in the config-file tree
    /// (viper lowercases config keys; 13 §23).
    fn file_lookup(&self, key: &str) -> Option<&Value> {
        let obj = self.file.as_object()?;
        obj.iter()
            .find(|(k, _)| k.to_ascii_lowercase().replace('_', "-") == key)
            .map(|(_, v)| v)
    }

    /// True if the CLI layer explicitly set `key`.
    fn cli_set(&self, key: &str) -> bool {
        matches!(self.cli.value_source(key), Some(ValueSource::CommandLine))
    }

    fn parse_err(
        key: &str,
        value: impl std::fmt::Display,
        want: &'static str,
        msg: impl std::fmt::Display,
    ) -> ConfigError {
        ConfigError::InvalidFlagValue {
            key: key.to_string(),
            value: value.to_string(),
            want,
            msg: msg.to_string(),
        }
    }

    /// Resolves `key` as a string (String-kinded flags).
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownKey`] / [`ConfigError::InvalidFlagValue`].
    pub fn get_string(&self, key: &str) -> crate::Result<String> {
        let spec = self.spec(key)?;
        if self.cli_set(key) {
            return Ok(self.cli.get_one::<String>(key).cloned().unwrap_or_default());
        }
        if let Some(v) = self.env.get(key) {
            return Ok(v.clone());
        }
        if let Some(v) = self.file_lookup(key) {
            return Ok(match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            });
        }
        Ok(spec.default.resolve())
    }

    /// [`Self::get_string`] + `getExpandedArg` path expansion
    /// (`$AVALANCHEGO_DATA_DIR` → resolved data-dir; other `$VAR` → OS env).
    ///
    /// # Errors
    ///
    /// See [`Self::get_string`].
    pub fn get_expanded_string(&self, key: &str) -> crate::Result<String> {
        Ok(self.expand(&self.get_string(key)?))
    }

    /// `os.Expand` with the Go mapper: `$AVALANCHEGO_DATA_DIR` → the resolved
    /// (and itself OS-expanded) `data-dir` value; anything else → OS env.
    fn expand(&self, s: &str) -> String {
        os_expand(s, &|name: &str| {
            if name == DATA_DIR_VAR {
                let data_dir = self.get_string(keys::KEY_DATA_DIR).unwrap_or_default();
                return os_expand(&data_dir, &|n: &str| {
                    self.os_env.get(n).cloned().unwrap_or_default()
                });
            }
            self.os_env.get(name).cloned().unwrap_or_default()
        })
    }

    /// Resolves `key` as a bool (Go `strconv.ParseBool` forms on the
    /// env/file/default layers).
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownKey`] / [`ConfigError::InvalidFlagValue`].
    pub fn get_bool(&self, key: &str) -> crate::Result<bool> {
        let spec = self.spec(key)?;
        if self.cli_set(key) {
            return Ok(self.cli.get_one::<bool>(key).copied().unwrap_or_default());
        }
        if let Some(v) = self.env.get(key) {
            return parse_bool_go(v).ok_or_else(|| Self::parse_err(key, v, "bool", "not a bool"));
        }
        if let Some(v) = self.file_lookup(key) {
            return match v {
                Value::Bool(b) => Ok(*b),
                Value::String(s) => {
                    parse_bool_go(s).ok_or_else(|| Self::parse_err(key, s, "bool", "not a bool"))
                }
                other => Err(Self::parse_err(key, other, "bool", "not a bool")),
            };
        }
        let d = spec.default.resolve();
        parse_bool_go(&d).ok_or_else(|| Self::parse_err(key, &d, "bool", "bad default"))
    }

    /// Resolves `key` as a `u64` (uint/uint64 flags).
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownKey`] / [`ConfigError::InvalidFlagValue`].
    pub fn get_u64(&self, key: &str) -> crate::Result<u64> {
        let spec = self.spec(key)?;
        if self.cli_set(key) {
            let raw = self.cli.get_one::<String>(key).cloned().unwrap_or_default();
            return raw
                .parse()
                .map_err(|e| Self::parse_err(key, &raw, "u64", e));
        }
        if let Some(v) = self.env.get(key) {
            return v.parse().map_err(|e| Self::parse_err(key, v, "u64", e));
        }
        if let Some(v) = self.file_lookup(key) {
            return match v {
                Value::Number(n) => n
                    .as_u64()
                    .ok_or_else(|| Self::parse_err(key, n, "u64", "out of range")),
                Value::String(s) => s.parse().map_err(|e| Self::parse_err(key, s, "u64", e)),
                other => Err(Self::parse_err(key, other, "u64", "not a number")),
            };
        }
        let d = spec.default.resolve();
        d.parse().map_err(|e| Self::parse_err(key, &d, "u64", e))
    }

    /// Resolves `key` as an `i64` (int flags).
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownKey`] / [`ConfigError::InvalidFlagValue`].
    pub fn get_i64(&self, key: &str) -> crate::Result<i64> {
        let spec = self.spec(key)?;
        if self.cli_set(key) {
            let raw = self.cli.get_one::<String>(key).cloned().unwrap_or_default();
            return raw
                .parse()
                .map_err(|e| Self::parse_err(key, &raw, "i64", e));
        }
        if let Some(v) = self.env.get(key) {
            return v.parse().map_err(|e| Self::parse_err(key, v, "i64", e));
        }
        if let Some(v) = self.file_lookup(key) {
            return match v {
                Value::Number(n) => n
                    .as_i64()
                    .ok_or_else(|| Self::parse_err(key, n, "i64", "out of range")),
                Value::String(s) => s.parse().map_err(|e| Self::parse_err(key, s, "i64", e)),
                other => Err(Self::parse_err(key, other, "i64", "not a number")),
            };
        }
        let d = spec.default.resolve();
        d.parse().map_err(|e| Self::parse_err(key, &d, "i64", e))
    }

    /// Resolves `key` as an `f64` (float64 flags).
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownKey`] / [`ConfigError::InvalidFlagValue`].
    pub fn get_f64(&self, key: &str) -> crate::Result<f64> {
        let spec = self.spec(key)?;
        if self.cli_set(key) {
            let raw = self.cli.get_one::<String>(key).cloned().unwrap_or_default();
            return raw
                .parse()
                .map_err(|e| Self::parse_err(key, &raw, "f64", e));
        }
        if let Some(v) = self.env.get(key) {
            return v.parse().map_err(|e| Self::parse_err(key, v, "f64", e));
        }
        if let Some(v) = self.file_lookup(key) {
            return match v {
                Value::Number(n) => n
                    .as_f64()
                    .ok_or_else(|| Self::parse_err(key, n, "f64", "out of range")),
                Value::String(s) => s.parse().map_err(|e| Self::parse_err(key, s, "f64", e)),
                other => Err(Self::parse_err(key, other, "f64", "not a number")),
            };
        }
        let d = spec.default.resolve();
        d.parse().map_err(|e| Self::parse_err(key, &d, "f64", e))
    }

    /// Resolves `key` as a duration. Strings use the Go `time.ParseDuration`
    /// grammar; bare numbers (file layer) are nanoseconds, as in Go's `cast`.
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownKey`] / [`ConfigError::InvalidFlagValue`] / the
    /// duration grammar errors.
    pub fn get_duration(&self, key: &str) -> crate::Result<std::time::Duration> {
        let spec = self.spec(key)?;
        if self.cli_set(key) {
            return Ok(self
                .cli
                .get_one::<std::time::Duration>(key)
                .copied()
                .unwrap_or_default());
        }
        if let Some(v) = self.env.get(key) {
            return crate::duration::parse_go_duration(v);
        }
        if let Some(v) = self.file_lookup(key) {
            return match v {
                Value::String(s) => crate::duration::parse_go_duration(s),
                Value::Number(n) => {
                    let nanos = n
                        .as_u64()
                        .ok_or_else(|| Self::parse_err(key, n, "duration", "out of range"))?;
                    Ok(std::time::Duration::from_nanos(nanos))
                }
                other => Err(Self::parse_err(key, other, "duration", "not a duration")),
            };
        }
        crate::duration::parse_go_duration(&spec.default.resolve())
    }

    /// Resolves `key` as a string slice (comma-separated on the CLI/env/string
    /// layers, arrays in the file layer, pflag `[a,b]` form for defaults).
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownKey`] / [`ConfigError::InvalidFlagValue`].
    pub fn get_string_slice(&self, key: &str) -> crate::Result<Vec<String>> {
        let spec = self.spec(key)?;
        if self.cli_set(key) {
            return Ok(self
                .cli
                .get_many::<String>(key)
                .map(|vals| vals.cloned().collect())
                .unwrap_or_default());
        }
        if let Some(v) = self.env.get(key) {
            return Ok(split_comma(v));
        }
        if let Some(v) = self.file_lookup(key) {
            return match v {
                Value::Array(items) => items
                    .iter()
                    .map(|item| match item {
                        Value::String(s) => Ok(s.clone()),
                        other => Ok(other.to_string()),
                    })
                    .collect(),
                Value::String(s) => Ok(split_comma(s)),
                other => Err(Self::parse_err(key, other, "stringSlice", "not a list")),
            };
        }
        Ok(parse_pflag_slice(&spec.default.resolve()))
    }

    /// Resolves `key` as an int slice.
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownKey`] / [`ConfigError::InvalidFlagValue`].
    pub fn get_int_slice(&self, key: &str) -> crate::Result<Vec<i64>> {
        self.get_string_slice(key)?
            .iter()
            .map(|s| {
                s.trim()
                    .parse()
                    .map_err(|e| Self::parse_err(key, s, "intSlice", e))
            })
            .collect()
    }

    /// Resolves `key` as a `k=v` string map (pflag `stringToString`).
    ///
    /// # Errors
    ///
    /// [`ConfigError::UnknownKey`] / [`ConfigError::InvalidFlagValue`].
    pub fn get_string_map(&self, key: &str) -> crate::Result<HashMap<String, String>> {
        let spec = self.spec(key)?;
        let from_pairs = |pairs: Vec<String>| -> crate::Result<HashMap<String, String>> {
            pairs
                .iter()
                .map(|pair| {
                    pair.split_once('=')
                        .map(|(k, v)| (k.to_string(), v.to_string()))
                        .ok_or_else(|| Self::parse_err(key, pair, "stringToString", "missing '='"))
                })
                .collect()
        };
        if self.cli_set(key) {
            let pairs: Vec<String> = self
                .cli
                .get_many::<String>(key)
                .map(|vals| vals.cloned().collect())
                .unwrap_or_default();
            return from_pairs(pairs.iter().flat_map(|p| split_comma(p)).collect());
        }
        if let Some(v) = self.env.get(key) {
            return from_pairs(split_comma(v));
        }
        if let Some(v) = self.file_lookup(key) {
            return match v {
                Value::Object(obj) => Ok(obj
                    .iter()
                    .map(|(k, val)| {
                        let val = match val {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        (k.clone(), val)
                    })
                    .collect()),
                Value::String(s) => from_pairs(split_comma(s)),
                other => Err(Self::parse_err(key, other, "stringToString", "not a map")),
            };
        }
        // pflag stringToString DefValue is `[]` (or `[k=v,…]`).
        from_pairs(parse_pflag_slice(&spec.default.resolve()))
    }
}

/// Loads a config file from an explicit (already-expanded) path, with the
/// `-content` override still applied first.
fn load_config_file_at(
    cli: &ArgMatches,
    env: &HashMap<String, String>,
    path: &str,
) -> crate::Result<Value> {
    if cli_or_env(cli, env, keys::KEY_CONFIG_FILE_CONTENT).is_some() {
        return load_config_file(cli, env);
    }
    let format = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let format = if format == "yml" {
        "yaml".to_string()
    } else {
        format
    };
    let raw = std::fs::read(path).map_err(|e| ConfigError::ConfigFileRead {
        path: path.to_string(),
        msg: e.to_string(),
    })?;
    parse_config_bytes(&raw, &format)
}

/// Go `strconv.ParseBool`: 1/t/T/TRUE/true/True and 0/f/F/FALSE/false/False.
fn parse_bool_go(s: &str) -> Option<bool> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Some(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Some(false),
        _ => None,
    }
}

/// Comma-split, dropping empties (`""` → `[]`).
fn split_comma(s: &str) -> Vec<String> {
    if s.is_empty() {
        return Vec::new();
    }
    s.split(',').map(str::to_string).collect()
}

/// Parses pflag's slice `DefValue` rendering: `[]` → empty, `[a,b]` → values.
fn parse_pflag_slice(s: &str) -> Vec<String> {
    let inner = s
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(s);
    split_comma(inner)
}

/// `os.Expand` for `$name` / `${name}` (alphanumeric + `_` names; unknown
/// names map to `""` like `os.Getenv`; a lone `$` is kept literal).
fn os_expand(s: &str, lookup: &dyn Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find('$') {
        out.push_str(rest.get(..idx).unwrap_or(""));
        let after = rest.get(idx.saturating_add(1)..).unwrap_or("");
        if let Some(braced) = after.strip_prefix('{') {
            if let Some(end) = braced.find('}') {
                out.push_str(&lookup(braced.get(..end).unwrap_or("")));
                rest = braced.get(end.saturating_add(1)..).unwrap_or("");
                continue;
            }
            // Unterminated `${`: keep literally.
            out.push('$');
            rest = after;
            continue;
        }
        let name_len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        if name_len == 0 {
            out.push('$');
            rest = after;
            continue;
        }
        out.push_str(&lookup(after.get(..name_len).unwrap_or("")));
        rest = after.get(name_len..).unwrap_or("");
    }
    out.push_str(rest);
    out
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
    fn data_dir_expansion() {
        use crate::flags::{FLAG_SPECS, build_command};
        use crate::keys;

        // $AVALANCHEGO_DATA_DIR expands to the RESOLVED data-dir; other $VARs
        // expand via the OS environment (Go flags.go::getExpandedArg, 13 §0).
        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            ["avalanchers".to_string(), "--data-dir=/base".to_string()],
            FLAG_SPECS,
            [("MYVAR".to_string(), "/mnt/x".to_string())].into_iter(),
        )
        .expect("build");
        // Default db-dir is the literal `$AVALANCHEGO_DATA_DIR/db`.
        assert_eq!(
            layered.get_string(keys::KEY_DB_DIR).expect("db-dir"),
            "$AVALANCHEGO_DATA_DIR/db"
        );
        assert_eq!(
            layered
                .get_expanded_string(keys::KEY_DB_DIR)
                .expect("db-dir"),
            "/base/db"
        );

        // Any other $VAR resolves from the (snapshotted) OS env; unknown vars
        // expand to "" like os.Getenv.
        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            [
                "avalanchers".to_string(),
                "--log-dir=$MYVAR/logs".to_string(),
                "--db-dir=${MYVAR}/db".to_string(),
                "--plugin-dir=$NOPE/p".to_string(),
            ],
            FLAG_SPECS,
            [("MYVAR".to_string(), "/mnt/x".to_string())].into_iter(),
        )
        .expect("build");
        assert_eq!(
            layered.get_expanded_string(keys::KEY_LOG_DIR).expect("log"),
            "/mnt/x/logs"
        );
        assert_eq!(
            layered.get_expanded_string(keys::KEY_DB_DIR).expect("db"),
            "/mnt/x/db"
        );
        assert_eq!(
            layered
                .get_expanded_string(keys::KEY_PLUGIN_DIR)
                .expect("plugin"),
            "/p"
        );

        // The data-dir value itself gets OS-env expansion ($HOME default).
        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            ["avalanchers".to_string()],
            FLAG_SPECS,
            [("HOME".to_string(), "/home/u".to_string())].into_iter(),
        )
        .expect("build");
        assert_eq!(
            layered
                .get_expanded_string(keys::KEY_CHAIN_DATA_DIR)
                .expect("chain-data-dir"),
            "/home/u/.avalanchego/chainData"
        );
    }

    #[test]
    fn is_set_layers() {
        use base64::Engine as _;

        use crate::flags::{FLAG_SPECS, build_command};
        use crate::keys;

        // Default only → not set, default returned.
        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            ["avalanchers".to_string()],
            FLAG_SPECS,
            std::iter::empty(),
        )
        .expect("build");
        assert!(!layered.is_set(keys::KEY_HTTP_PORT));
        assert_eq!(layered.get_u64(keys::KEY_HTTP_PORT).expect("port"), 9650);

        // CLI layer.
        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            ["avalanchers".to_string(), "--http-port=1".to_string()],
            FLAG_SPECS,
            std::iter::empty(),
        )
        .expect("build");
        assert!(layered.is_set(keys::KEY_HTTP_PORT));
        assert_eq!(layered.get_u64(keys::KEY_HTTP_PORT).expect("port"), 1);

        // Env layer.
        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            ["avalanchers".to_string()],
            FLAG_SPECS,
            [("AVAGO_HTTP_PORT".to_string(), "2".to_string())].into_iter(),
        )
        .expect("build");
        assert!(layered.is_set(keys::KEY_HTTP_PORT));
        assert_eq!(layered.get_u64(keys::KEY_HTTP_PORT).expect("port"), 2);

        // File layer (via --config-file-content), incl. case-insensitive and
        // '_'→'-' key folding (13 §23).
        let content = base64::engine::general_purpose::STANDARD
            .encode(r#"{"HTTP_PORT": 3, "log-level": "debug"}"#);
        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            [
                "avalanchers".to_string(),
                format!("--config-file-content={content}"),
            ],
            FLAG_SPECS,
            std::iter::empty(),
        )
        .expect("build");
        assert!(layered.is_set(keys::KEY_HTTP_PORT));
        assert_eq!(layered.get_u64(keys::KEY_HTTP_PORT).expect("port"), 3);
        assert!(layered.is_set(keys::KEY_LOG_LEVEL));
        assert_eq!(
            layered.get_string(keys::KEY_LOG_LEVEL).expect("level"),
            "debug"
        );
        assert!(!layered.is_set(keys::KEY_LOG_DISPLAY_LEVEL));
    }

    #[test]
    fn typed_getters_walk_all_layers() {
        use crate::flags::{FLAG_SPECS, build_command};
        use crate::keys;

        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            [
                "avalanchers".to_string(),
                "--sybil-protection-enabled=false".to_string(),
                "--network-ping-timeout=22.5s".to_string(),
                "--tracing-sample-rate=0.25".to_string(),
                "--snow-sample-size=11".to_string(),
                "--http-allowed-hosts=a,b".to_string(),
                "--acp-support=7,8".to_string(),
                "--tracing-headers=k1=v1".to_string(),
                "--tracing-headers=k2=v2".to_string(),
            ],
            FLAG_SPECS,
            [("AVAGO_BENCHLIST_DURATION".to_string(), "7m".to_string())].into_iter(),
        )
        .expect("build");
        assert!(
            !layered
                .get_bool(keys::KEY_SYBIL_PROTECTION_ENABLED)
                .expect("bool")
        );
        assert_eq!(
            layered
                .get_duration(keys::KEY_NETWORK_PING_TIMEOUT)
                .expect("dur"),
            std::time::Duration::from_millis(22_500)
        );
        // Env-layer duration.
        assert_eq!(
            layered
                .get_duration(keys::KEY_BENCHLIST_DURATION)
                .expect("dur"),
            std::time::Duration::from_secs(420)
        );
        assert!(
            (layered.get_f64(keys::KEY_TRACING_SAMPLE_RATE).expect("f64") - 0.25).abs() < 1e-12
        );
        assert_eq!(
            layered.get_i64(keys::KEY_SNOW_SAMPLE_SIZE).expect("i64"),
            11
        );
        assert_eq!(
            layered
                .get_string_slice(keys::KEY_HTTP_ALLOWED_HOSTS)
                .expect("slice"),
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(
            layered.get_int_slice(keys::KEY_ACP_SUPPORT).expect("ints"),
            vec![7, 8]
        );
        let map = layered
            .get_string_map(keys::KEY_TRACING_HEADERS)
            .expect("map");
        assert_eq!(map.get("k1").map(String::as_str), Some("v1"));
        assert_eq!(map.get("k2").map(String::as_str), Some("v2"));

        // Defaults for every kind still resolve (slice/map defaults parse the
        // pflag `[a,b]` DefValue form).
        let layered = Layered::build_with_env(
            build_command(FLAG_SPECS),
            ["avalanchers".to_string()],
            FLAG_SPECS,
            std::iter::empty(),
        )
        .expect("build");
        assert_eq!(
            layered
                .get_string_slice(keys::KEY_HTTP_ALLOWED_HOSTS)
                .expect("slice"),
            vec!["localhost".to_string()]
        );
        assert_eq!(
            layered.get_int_slice(keys::KEY_ACP_SUPPORT).expect("ints"),
            Vec::<i64>::new()
        );
        assert!(
            layered
                .get_string_map(keys::KEY_TRACING_HEADERS)
                .expect("map")
                .is_empty()
        );
        assert_eq!(
            layered
                .get_duration(keys::KEY_NETWORK_PING_FREQUENCY)
                .expect("dur"),
            std::time::Duration::from_millis(22_500)
        );
        assert!(layered.get_bool(keys::KEY_API_INFO_ENABLED).expect("bool"));
        assert_eq!(layered.get_u64(keys::KEY_TX_FEE).expect("u64"), 1_000_000);
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
