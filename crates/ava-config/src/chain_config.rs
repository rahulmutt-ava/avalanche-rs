// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Chain config dir/content loaders + alias-file loaders
//! (Go `config/config.go::getChainConfigs`/`getAliases`,
//! specs 12 §1.7, 13 §14).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ava_types::id::Id;
use base64::Engine as _;
use serde::Deserialize;

use crate::error::ConfigError;
use crate::keys;
use crate::precedence::Layered;

/// Per-chain config file basename (Go `chainConfigFileName`).
const CHAIN_CONFIG_FILE_NAME: &str = "config";
/// Per-chain upgrade file basename (Go `chainUpgradeFileName`).
const CHAIN_UPGRADE_FILE_NAME: &str = "upgrade";

/// Per-chain config + upgrade blobs (Go `chains.ChainConfig`). The blobs are
/// opaque to the config layer; each VM parses its own (13 §14).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChainConfig {
    /// The user-provided config blob for the chain (`Config`).
    pub config: Vec<u8>,
    /// The chain-specific upgrade-coordination blob (`Upgrade`).
    pub upgrade: Vec<u8>,
}

/// Deserializes a Go `[]byte` JSON value (a std-base64 string; absent → empty).
fn de_b64_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> core::result::Result<Vec<u8>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    match s {
        None => Ok(Vec::new()),
        Some(s) => base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .map_err(serde::de::Error::custom),
    }
}

/// The `--chain-config-content` JSON map value shape. Go's `chains.ChainConfig`
/// has no JSON tags, so the canonical field names are `Config`/`Upgrade`;
/// Go's unmarshalling is case-insensitive, so the lowercase aliases are also
/// accepted.
#[derive(Default, Deserialize)]
#[serde(default)]
struct RawChainConfig {
    #[serde(rename = "Config", alias = "config", deserialize_with = "de_b64_bytes")]
    config: Vec<u8>,
    #[serde(
        rename = "Upgrade",
        alias = "upgrade",
        deserialize_with = "de_b64_bytes"
    )]
    upgrade: Vec<u8>,
}

/// Go `getPathFromDirKey` — the expanded dir for `config_key`:
/// `Some(path)` when it exists, `None` when unset-and-missing, and
/// [`ConfigError::CannotReadDirectory`] when explicitly set but missing.
///
/// # Errors
/// [`ConfigError::CannotReadDirectory`] / flag-read failures.
pub(crate) fn path_from_dir_key(
    layered: &Layered,
    config_key: &str,
) -> crate::Result<Option<PathBuf>> {
    let config_dir = layered.get_expanded_string(config_key)?;
    let clean_path = PathBuf::from(&config_dir);
    if clean_path.is_dir() {
        return Ok(Some(clean_path));
    }
    if layered.is_set(config_key) {
        // The user specified a config dir explicitly, but it does not exist.
        return Err(ConfigError::CannotReadDirectory {
            path: clean_path.display().to_string(),
        });
    }
    Ok(None)
}

/// Go `storage.ReadFileWithName` — reads the single `<parent>/<name>.*` file
/// (any extension); none → empty, more than one → error.
fn read_file_with_name(parent_dir: &Path, file_name_no_ext: &str) -> crate::Result<Vec<u8>> {
    let mut matched: Option<PathBuf> = None;
    let entries = std::fs::read_dir(parent_dir).map_err(|e| ConfigError::ConfigFileRead {
        path: parent_dir.display().to_string(),
        msg: e.to_string(),
    })?;
    for entry in entries {
        let entry = entry.map_err(|e| ConfigError::ConfigFileRead {
            path: parent_dir.display().to_string(),
            msg: e.to_string(),
        })?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        // Glob `<name>.*`: the basename plus a dot plus any (possibly empty
        // span of) characters.
        let Some(rest) = name.strip_prefix(file_name_no_ext) else {
            continue;
        };
        if !rest.starts_with('.') {
            continue;
        }
        if matched.is_some() {
            return Err(ConfigError::ConfigFileRead {
                path: parent_dir.display().to_string(),
                msg: format!(
                    "too many files matched \"{file_name_no_ext}.*\" in {}",
                    parent_dir.display()
                ),
            });
        }
        matched = Some(entry.path());
    }
    let Some(path) = matched else {
        return Ok(Vec::new());
    };
    std::fs::read(&path).map_err(|e| ConfigError::ConfigFileRead {
        path: path.display().to_string(),
        msg: e.to_string(),
    })
}

/// Go `getChainConfigs` — `--chain-config-content` (b64 JSON map) when set,
/// else the `<chain-config-dir>/<alias>/{config,upgrade}.*` layout (13 §14).
///
/// # Errors
/// [`ConfigError::InvalidBase64Content`] / [`ConfigError::Unmarshalling`] /
/// [`ConfigError::CannotReadDirectory`] / read failures.
pub fn get_chain_configs(layered: &Layered) -> crate::Result<HashMap<String, ChainConfig>> {
    if layered.is_set(keys::KEY_CHAIN_CONFIG_CONTENT) {
        return get_chain_configs_from_flag(layered);
    }
    get_chain_configs_from_dir(layered)
}

/// Go `getChainConfigsFromFlag`.
fn get_chain_configs_from_flag(layered: &Layered) -> crate::Result<HashMap<String, ChainConfig>> {
    let content_b64 = layered.get_string(keys::KEY_CHAIN_CONFIG_CONTENT)?;
    let content = base64::engine::general_purpose::STANDARD
        .decode(content_b64.trim())
        .map_err(|e| ConfigError::InvalidBase64Content { msg: e.to_string() })?;
    let raw: HashMap<String, RawChainConfig> =
        serde_json::from_slice(&content).map_err(|e| ConfigError::Unmarshalling {
            what: "chain configs".to_string(),
            msg: e.to_string(),
        })?;
    Ok(raw
        .into_iter()
        .map(|(alias, c)| {
            (
                alias,
                ChainConfig {
                    config: c.config,
                    upgrade: c.upgrade,
                },
            )
        })
        .collect())
}

/// Go `getChainConfigsFromDir`.
fn get_chain_configs_from_dir(layered: &Layered) -> crate::Result<HashMap<String, ChainConfig>> {
    let Some(chain_config_path) = path_from_dir_key(layered, keys::KEY_CHAIN_CONFIG_DIR)? else {
        return Ok(HashMap::new());
    };
    read_chain_config_path(&chain_config_path)
}

/// Go `readChainConfigPath` — every sub*directory* of `chain_config_path`
/// yields a `ChainConfig` keyed by the directory name; non-directories are
/// skipped.
fn read_chain_config_path(chain_config_path: &Path) -> crate::Result<HashMap<String, ChainConfig>> {
    let mut chain_configs = HashMap::new();
    let entries =
        std::fs::read_dir(chain_config_path).map_err(|e| ConfigError::ConfigFileRead {
            path: chain_config_path.display().to_string(),
            msg: e.to_string(),
        })?;
    for entry in entries {
        let entry = entry.map_err(|e| ConfigError::ConfigFileRead {
            path: chain_config_path.display().to_string(),
            msg: e.to_string(),
        })?;
        let chain_dir = entry.path();
        if !chain_dir.is_dir() {
            continue;
        }
        let config = read_file_with_name(&chain_dir, CHAIN_CONFIG_FILE_NAME)?;
        let upgrade = read_file_with_name(&chain_dir, CHAIN_UPGRADE_FILE_NAME)?;
        let Some(alias) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        chain_configs.insert(alias, ChainConfig { config, upgrade });
    }
    Ok(chain_configs)
}

/// Go `getAliases` — a JSON `map[ids.ID][]string` from the b64 `-content`
/// flag (which wins) or the alias file path; unset-and-missing → empty.
fn get_aliases(
    layered: &Layered,
    name: &str,
    content_key: &str,
    file_key: &str,
) -> crate::Result<HashMap<Id, Vec<String>>> {
    let file_bytes: Vec<u8> = if layered.is_set(content_key) {
        let content_b64 = layered.get_string(content_key)?;
        base64::engine::general_purpose::STANDARD
            .decode(content_b64.trim())
            .map_err(|e| ConfigError::InvalidBase64Content {
                msg: format!("for {name}: {e}"),
            })?
    } else {
        let alias_file_path = PathBuf::from(layered.get_expanded_string(file_key)?);
        if !alias_file_path.is_file() {
            if layered.is_set(file_key) {
                return Err(ConfigError::FileDoesNotExist {
                    path: alias_file_path.display().to_string(),
                });
            }
            return Ok(HashMap::new());
        }
        std::fs::read(&alias_file_path).map_err(|e| ConfigError::ConfigFileRead {
            path: alias_file_path.display().to_string(),
            msg: e.to_string(),
        })?
    };

    serde_json::from_slice(&file_bytes).map_err(|e| ConfigError::Unmarshalling {
        what: name.to_string(),
        msg: e.to_string(),
    })
}

/// Go `getVMAliases` — `map[vmID][]alias` (13 §14).
///
/// # Errors
/// See [`get_chain_aliases`].
pub fn get_vm_aliases(layered: &Layered) -> crate::Result<HashMap<Id, Vec<String>>> {
    get_aliases(
        layered,
        "vm aliases",
        keys::KEY_VM_ALIASES_FILE_CONTENT,
        keys::KEY_VM_ALIASES_FILE,
    )
}

/// Go `getChainAliases` — `map[blockchainID][]alias` (13 §14). Note the
/// loader gates on `--chain-aliases-file-content` (the flag-help "Ignored if
/// chain-config-content" text is a Go quirk; behavior wins).
///
/// # Errors
/// [`ConfigError::FileDoesNotExist`] when the path is explicitly set but
/// missing, [`ConfigError::InvalidBase64Content`] /
/// [`ConfigError::Unmarshalling`] on malformed content.
pub fn get_chain_aliases(layered: &Layered) -> crate::Result<HashMap<Id, Vec<String>>> {
    get_aliases(
        layered,
        "chain aliases",
        keys::KEY_CHAIN_ALIASES_FILE_CONTENT,
        keys::KEY_CHAIN_ALIASES_FILE,
    )
}

#[cfg(test)]
mod tests {
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

    fn b64(s: impl AsRef<[u8]>) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn chain_config_dir_layout() {
        // <dir>/<alias>/{config,upgrade}.* -> ChainConfig{config, upgrade}
        // (extension-agnostic, Go storage.ReadFileWithName; 13 §14).
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("C")).expect("mkdir C");
        std::fs::write(dir.path().join("C/config.json"), "cfg-c").expect("write");
        std::fs::write(dir.path().join("C/upgrade.json"), "up-c").expect("write");
        std::fs::create_dir(dir.path().join("X")).expect("mkdir X");
        std::fs::write(dir.path().join("X/config.ex"), "cfg-x").expect("write");
        // Non-directory entries are skipped.
        std::fs::write(dir.path().join("stray.json"), "ignored").expect("write");

        let l = layered(&[&format!("--chain-config-dir={}", dir.path().display())]);
        let configs = get_chain_configs(&l).expect("configs");
        assert_eq!(configs.len(), 2);
        let c = configs.get("C").expect("C");
        assert_eq!(c.config, b"cfg-c");
        assert_eq!(c.upgrade, b"up-c");
        let x = configs.get("X").expect("X");
        assert_eq!(x.config, b"cfg-x");
        assert!(x.upgrade.is_empty());

        // The b64 chain-config-content map form wins over the dir.
        let content = b64(format!(
            r#"{{"C":{{"Config":"{}","Upgrade":"{}"}}}}"#,
            b64("aaa"),
            b64("bbb")
        ));
        let l = layered(&[
            "--chain-config-dir=/definitely/not/a/dir",
            &format!("--chain-config-content={content}"),
        ]);
        let configs = get_chain_configs(&l).expect("configs");
        let c = configs.get("C").expect("C");
        assert_eq!(c.config, b"aaa");
        assert_eq!(c.upgrade, b"bbb");

        // Explicitly-set but missing dir -> errCannotReadDirectory.
        let l = layered(&["--chain-config-dir=/definitely/not/a/dir"]);
        assert_matches!(
            get_chain_configs(&l),
            Err(ConfigError::CannotReadDirectory { .. })
        );

        // Unset + missing default dir -> empty map.
        let data = tempfile::tempdir().expect("tempdir");
        let l = layered(&[&format!("--data-dir={}", data.path().display())]);
        assert!(get_chain_configs(&l).expect("configs").is_empty());
    }

    #[test]
    fn alias_file_loaders() {
        // JSON object mapping a cb58 ID to a list of aliases
        // (map[ids.ID][]string, 13 §14).
        let id = Id::from([7u8; 32]);
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("aliases.json");
        std::fs::write(&path, format!(r#"{{"{id}":["MyChain","mychain"]}}"#)).expect("write");

        let l = layered(&[&format!("--chain-aliases-file={}", path.display())]);
        let aliases = get_chain_aliases(&l).expect("aliases");
        assert_eq!(
            aliases.get(&id).expect("id"),
            &vec!["MyChain".to_string(), "mychain".to_string()]
        );

        // The b64 -content form wins over the file path (13 §14 quirk note).
        let content = b64(format!(r#"{{"{id}":["other"]}}"#));
        let l = layered(&[
            &format!("--chain-aliases-file={}", path.display()),
            &format!("--chain-aliases-file-content={content}"),
        ]);
        let aliases = get_chain_aliases(&l).expect("aliases");
        assert_eq!(aliases.get(&id).expect("id"), &vec!["other".to_string()]);

        // Explicitly-set but missing file -> errFileDoesNotExist.
        let l = layered(&["--chain-aliases-file=/definitely/not/a/file.json"]);
        assert_matches!(
            get_chain_aliases(&l),
            Err(ConfigError::FileDoesNotExist { .. })
        );

        // Unset + missing -> no aliases.
        let data = tempfile::tempdir().expect("tempdir");
        let l = layered(&[&format!("--data-dir={}", data.path().display())]);
        assert!(get_chain_aliases(&l).expect("aliases").is_empty());

        // VM aliases share the loader (--vm-aliases-file).
        let l = layered(&[&format!("--vm-aliases-file={}", path.display())]);
        let aliases = get_vm_aliases(&l).expect("aliases");
        assert_eq!(aliases.get(&id).expect("id").len(), 2);
    }
}
