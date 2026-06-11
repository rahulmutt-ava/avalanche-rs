// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Args/Reply types for the `admin` API, field-for-field with Go
//! `api/admin/service.go` (json tags, level casing, `errorCode` numbers —
//! specs 14 §4).

use std::collections::BTreeMap;

use ava_logging::AvaLevel;
use ava_types::id::Id;

/// A parameterless method's Args (Go `*struct{}`): any object (or absent
/// params) deserializes; unknown fields are ignored, exactly like `encoding/json`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct EmptyArgs {}

/// Go `api.EmptyReply` — serializes as `{}`.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct EmptyReply {}

/// Go `logging.Level` JSON behavior wrapped around [`AvaLevel`]: marshals as
/// the **uppercase** name (`"INFO"`, Go `Level.String()`), unmarshals
/// case-insensitively (Go `ToLevel` upper-cases its input).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LevelJson(pub AvaLevel);

impl serde::Serialize for LevelJson {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.as_upper_str())
    }
}

impl<'de> serde::Deserialize<'de> for LevelJson {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse::<AvaLevel>()
            .map(LevelJson)
            // Go `ToLevel`: `unknown log level: "<input>"`.
            .map_err(|_| serde::de::Error::custom(format!("unknown log level: {s:?}")))
    }
}

/// Go `AliasArgs`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AliasArgs {
    /// The endpoint to alias (e.g. `bc/X`).
    pub endpoint: String,
    /// The new alias (max 512 bytes).
    pub alias: String,
}

/// Go `AliasChainArgs`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AliasChainArgs {
    /// The chain to alias (an alias or stringified id).
    pub chain: String,
    /// The new chain alias (max 512 bytes).
    pub alias: String,
}

/// Go `GetChainAliasesArgs`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GetChainAliasesArgs {
    /// The chain's id (parsed as `ids.ID`, i.e. CB58 + checksum).
    pub chain: String,
}

/// Go `GetChainAliasesReply`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct GetChainAliasesReply {
    /// The aliases of the chain.
    pub aliases: Vec<String>,
}

/// Go `SetLoggerLevelArgs`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SetLoggerLevelArgs {
    /// The logger to mutate; empty means **all** loggers.
    #[serde(rename = "loggerName")]
    pub logger_name: String,
    /// The new file ("log") level; `None` leaves it unchanged.
    #[serde(rename = "logLevel")]
    pub log_level: Option<LevelJson>,
    /// The new display level; `None` leaves it unchanged.
    #[serde(rename = "displayLevel")]
    pub display_level: Option<LevelJson>,
}

/// Go `GetLoggerLevelArgs`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GetLoggerLevelArgs {
    /// The logger to read; empty means **all** loggers.
    #[serde(rename = "loggerName")]
    pub logger_name: String,
}

/// Go `LogAndDisplayLevels`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LogAndDisplayLevels {
    /// The file ("log") level.
    #[serde(rename = "logLevel")]
    pub log_level: LevelJson,
    /// The display level.
    #[serde(rename = "displayLevel")]
    pub display_level: LevelJson,
}

/// Go `LoggerLevelReply`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct LoggerLevelReply {
    /// Levels per logger name (sorted map; Go's map key order is irrelevant
    /// to clients, ours is deterministic — 00 §6.1).
    #[serde(rename = "loggerLevels")]
    pub logger_levels: BTreeMap<String, LogAndDisplayLevels>,
}

/// Go `LoadVMsReply`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct LoadVmsReply {
    /// VMs and their aliases which were successfully loaded. Keys serialize as
    /// `ids.ID` strings (CB58 + checksum), matching Go's `MarshalText`.
    #[serde(rename = "newVMs")]
    pub new_vms: BTreeMap<Id, Vec<String>>,
    /// VMs that failed to be loaded and the error message (Go `omitempty`:
    /// an empty map is omitted from the JSON).
    #[serde(
        rename = "failedVMs",
        skip_serializing_if = "BTreeMap::is_empty",
        default
    )]
    pub failed_vms: BTreeMap<Id, String>,
}

/// Go `DBGetArgs`.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DbGetArgs {
    /// The key, HexNC-encoded (`0x…`, no checksum; the empty string is the
    /// empty key — Go `formatting.Decode` returns nil bytes for `""`).
    pub key: String,
}

/// Go `DBGetReply`. `errorCode` serializes as a **number** (Go marshals the
/// `rpcdbpb.Error` int32 enum without a custom marshaler).
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct DbGetReply {
    /// The value, HexNC-encoded (`0x…`); the empty string when the read failed
    /// with a mapped error (Go leaves the field unset on that path).
    pub value: String,
    /// The `rpcdbpb.Error` enum value — see [`db_error_code`].
    #[serde(rename = "errorCode")]
    pub error_code: u32,
}

/// The `rpcdbpb.Error` enum values `dbGet` reports (proto `rpcdb.Error`;
/// Go `database/rpcdb.ErrorToErrEnum`).
pub mod db_error_code {
    /// `ERROR_UNSPECIFIED` — no error occurred.
    pub const UNSPECIFIED: u32 = 0;
    /// `ERROR_CLOSED` — `database.ErrClosed`.
    pub const CLOSED: u32 = 1;
    /// `ERROR_NOT_FOUND` — `database.ErrNotFound`.
    pub const NOT_FOUND: u32 = 2;
}
