// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! P-Chain transaction & blockchain status enums (port of
//! `vms/platformvm/status/status.go`, specs 09).
//!
//! [`Status`] is the tx status reported by `getTxStatus`; it serializes to the
//! Go-compatible PascalCase JSON strings (`"Committed"`, `"Processing"`, …) and
//! carries the same numeric discriminants as the Go `Status uint32` constants.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `status.Status` — the lifecycle state of a P-Chain transaction.
///
/// The numeric discriminants match Go exactly (`Unknown=0`, `Committed=4`,
/// `Aborted=5`, `Processing=6`, `Dropped=8`); JSON uses the PascalCase names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
#[repr(u32)]
pub enum Status {
    /// The transaction is not known.
    #[default]
    Unknown = 0,
    /// The transaction was proposed and committed.
    Committed = 4,
    /// The transaction was proposed and aborted.
    Aborted = 5,
    /// The transaction is currently in the preferred chain (or mempool).
    Processing = 6,
    /// The transaction was dropped due to failing verification.
    Dropped = 8,
}

impl Status {
    /// The Go `String()` rendering (also the JSON form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Status::Unknown => "Unknown",
            Status::Committed => "Committed",
            Status::Aborted => "Aborted",
            Status::Processing => "Processing",
            Status::Dropped => "Dropped",
        }
    }
}

impl fmt::Display for Status {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for Status {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Status {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "Unknown" => Ok(Status::Unknown),
            "Committed" => Ok(Status::Committed),
            "Aborted" => Ok(Status::Aborted),
            "Processing" => Ok(Status::Processing),
            "Dropped" => Ok(Status::Dropped),
            other => Err(serde::de::Error::custom(format!(
                "unknown status {other:?}"
            ))),
        }
    }
}

/// `status.BlockchainStatus` — reported by `getBlockchainStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum BlockchainStatus {
    /// The blockchain is not known.
    #[default]
    UnknownChain,
    /// The blockchain is being created (preferred but not yet accepted).
    Preferred,
    /// The blockchain was created and is validated by this node.
    Validating,
    /// The blockchain was created and is being synced by this node.
    Syncing,
}

impl BlockchainStatus {
    /// The Go `String()` rendering (also the JSON form).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            BlockchainStatus::UnknownChain => "Unknown",
            BlockchainStatus::Preferred => "Preferred",
            BlockchainStatus::Validating => "Validating",
            BlockchainStatus::Syncing => "Syncing",
        }
    }
}

impl fmt::Display for BlockchainStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Serialize for BlockchainStatus {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for BlockchainStatus {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "Unknown" => Ok(BlockchainStatus::UnknownChain),
            "Preferred" => Ok(BlockchainStatus::Preferred),
            "Validating" => Ok(BlockchainStatus::Validating),
            "Syncing" => Ok(BlockchainStatus::Syncing),
            other => Err(serde::de::Error::custom(format!(
                "unknown blockchain status {other:?}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_json_roundtrip() {
        for (s, json) in [
            (Status::Unknown, "\"Unknown\""),
            (Status::Committed, "\"Committed\""),
            (Status::Aborted, "\"Aborted\""),
            (Status::Processing, "\"Processing\""),
            (Status::Dropped, "\"Dropped\""),
        ] {
            let encoded = serde_json::to_string(&s).expect("encode");
            assert_eq!(encoded, json, "status {s} json mismatch");
            let decoded: Status = serde_json::from_str(json).expect("decode");
            assert_eq!(decoded, s);
        }
    }

    #[test]
    fn status_discriminants_match_go() {
        assert_eq!(Status::Unknown as u32, 0);
        assert_eq!(Status::Committed as u32, 4);
        assert_eq!(Status::Aborted as u32, 5);
        assert_eq!(Status::Processing as u32, 6);
        assert_eq!(Status::Dropped as u32, 8);
    }
}
