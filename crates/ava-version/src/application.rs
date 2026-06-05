// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `Application` version + the pinned client version constants.
//!
//! Mirrors `version/version.go` and `version/constants.go`.
//! Owning spec: `specs/03-core-primitives.md` §5.1, `specs/26-versioning-and-compatibility.md`.

use std::cmp::Ordering;
use std::sync::LazyLock;

/// The client name used on the network wire and in P2P handshakes.
///
/// The Rust port reports `"avalanchego"` as its name for drop-in interop —
/// see `specs/26` §2.2 for the rationale.
///
/// Mirrors Go `version/constants.go: Client = "avalanchego"`.
pub const CLIENT: &str = "avalanchego";

/// Alias for [`CLIENT`] — the application name string.
pub const APPLICATION_NAME: &str = CLIENT;

/// The rpcchainvm plugin protocol version.
///
/// MUST be exact-equality equal on both the host and the VM plugin side.
/// Bump this whenever a plugin must be rebuilt against the latest node.
///
/// Mirrors Go `version/constants.go: RPCChainVMProtocol = 45`.
pub const RPC_CHAIN_VM_PROTOCOL: u32 = 45;

/// The current on-disk database schema version (used as the database subdirectory name).
///
/// Mirrors Go `version/constants.go: CurrentDatabase = "v1.4.5"`.
pub const CURRENT_DATABASE: &str = "v1.4.5";

/// The previous on-disk database schema version (used for migration detection).
///
/// Mirrors Go `version/constants.go: PrevDatabase = "v1.0.0"`.
pub const PREV_DATABASE: &str = "v1.0.0";

/// The current avalanchego-compatible node version for this build.
///
/// This is the version reported in P2P handshakes and `info.getNodeVersion`.
/// The Rust port tracks the Go release it is wire-compatible with.
///
/// Mirrors Go `version/constants.go: Current = Application{...}` (1.14.2 at port time).
///
/// Note: `name` is always `"avalanchego"` (== [`CLIENT`]).
pub static CURRENT: LazyLock<Application> = LazyLock::new(|| Application {
    name: CLIENT.to_string(),
    major: 1,
    minor: 14,
    patch: 2,
});

/// The minimum peer version after the network upgrade time passes.
///
/// Mirrors Go `version/constants.go: MinimumCompatibleVersion = Application{...}` (1.14.0).
pub static MINIMUM_COMPATIBLE: LazyLock<Application> = LazyLock::new(|| Application {
    name: CLIENT.to_string(),
    major: 1,
    minor: 14,
    patch: 0,
});

/// The minimum peer version the current node accepts (before the upgrade time passes).
///
/// Mirrors Go `version/constants.go: PrevMinimumCompatibleVersion = Application{...}` (1.13.0).
pub static PREV_MINIMUM_COMPATIBLE: LazyLock<Application> = LazyLock::new(|| Application {
    name: CLIENT.to_string(),
    major: 1,
    minor: 13,
    patch: 0,
});

/// A node application version (major.minor.patch + client name).
///
/// Mirrors `version.Application` from Go (`version/version.go`). The `name` field
/// is the network-wire client identity string (e.g. `"avalanchego"`). The three
/// integer fields identify the version for the compatibility check.
///
/// **Ordering note:** [`Ord`] compares ONLY `(major, minor, patch)` — the `name`
/// field is intentionally excluded. This matches Go's `Application.Compare`
/// (`cmp.Compare` chain on the three integers). A peer with a different client name
/// but a compatible version triple is accepted at handshake.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Application {
    /// The client name (e.g. `"avalanchego"`). Not part of the ordering/compatibility check.
    pub name: String,
    /// Major version.
    pub major: u32,
    /// Minor version.
    pub minor: u32,
    /// Patch version.
    pub patch: u32,
}

impl Application {
    /// Constructs a new `Application` with the given fields.
    pub fn new(name: impl Into<String>, major: u32, minor: u32, patch: u32) -> Self {
        Self { name: name.into(), major, minor, patch }
    }

    /// Returns the display string `"<name>/<major>.<minor>.<patch>"`.
    ///
    /// MUST be byte-identical to Go's `Application.String()` —
    /// `fmt.Sprintf("%s/%d.%d.%d", name, major, minor, patch)`.
    /// This is the string carried in the P2P Handshake `Client` field and
    /// returned by `info.getNodeVersion.version`.
    pub fn display(&self) -> String {
        format!("{}/{}.{}.{}", self.name, self.major, self.minor, self.patch)
    }

    /// Returns the semantic version string `"v<major>.<minor>.<patch>"`.
    ///
    /// Mirrors Go `Application.Semantic()`.
    pub fn semantic(&self) -> String {
        format!("v{}.{}.{}", self.major, self.minor, self.patch)
    }

    /// Returns `"v<major>.<minor>.<patch>@<commit>"`, or just the semantic version
    /// if `git_commit` is empty.
    ///
    /// Mirrors Go `Application.SemanticWithCommit()`.
    pub fn semantic_with_commit(&self, git_commit: &str) -> String {
        if git_commit.is_empty() {
            self.semantic()
        } else {
            format!("{}@{}", self.semantic(), git_commit)
        }
    }
}

impl std::fmt::Display for Application {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}.{}.{}", self.name, self.major, self.minor, self.patch)
    }
}

/// Ordering compares only `(major, minor, patch)` — mirrors Go `Application.Compare`.
/// The `name` field is excluded (see struct-level doc).
impl Ord for Application {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl PartialOrd for Application {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
