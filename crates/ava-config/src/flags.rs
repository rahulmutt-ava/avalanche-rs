// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The flag-table model (specs 12 §1.4, 13 §25).
//!
//! Flags are declared as data ([`FlagSpec`]) so the `golden::flag_parity` test
//! can enumerate them and diff the generated set against the Go
//! `config.BuildFlagSet()` snapshot.

use ava_network::throttling::outbound_msg::{
    DEFAULT_AT_LARGE_ALLOC_SIZE, DEFAULT_NODE_MAX_AT_LARGE_BYTES, DEFAULT_VDR_ALLOC_SIZE,
};
use ava_snow::snowball::DEFAULT_PARAMETERS;
use clap::{Arg, ArgAction, Command};

use crate::defaults;
use crate::duration::{format_go_duration, parse_go_duration};
use crate::keys;

/// The pflag value type of a flag (Go `pflag.Value.Type()`).
///
/// Maps 1:1 onto the 10 pflag type strings that appear in the Go flag set
/// (specs 13 §25): `bool`, `string`, `int`, `uint`, `uint64`, `float64`,
/// `duration`, `intSlice`, `stringSlice`, `stringToString`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlagKind {
    /// Go `bool` (pflag accepts `--x` and `--x=true`).
    Bool,
    /// Go `string`.
    String,
    /// Go `uint64` → Rust `u64`.
    U64,
    /// Go `uint` → Rust `u32`/`u16` (port-sized values narrow at parse time).
    Uint,
    /// Go `int` → Rust `i32`.
    I64,
    /// Go `float64` → Rust `f64`.
    F64,
    /// Go `time.Duration` (pflag `duration`, `time.ParseDuration` grammar).
    Duration,
    /// Go `[]string` (pflag `stringSlice`, comma-separated).
    StringSlice,
    /// Go `[]int` (pflag `intSlice`, comma-separated).
    IntSlice,
    /// Go `map[string]string` (pflag `stringToString`, `k=v` pairs).
    StringMap,
}

impl FlagKind {
    /// The Go pflag `Value.Type()` string for this kind (specs 13 §25).
    #[must_use]
    pub const fn go_type_str(self) -> &'static str {
        match self {
            Self::Bool => "bool",
            Self::String => "string",
            Self::U64 => "uint64",
            Self::Uint => "uint",
            Self::I64 => "int",
            Self::F64 => "float64",
            Self::Duration => "duration",
            Self::StringSlice => "stringSlice",
            Self::IntSlice => "intSlice",
            Self::StringMap => "stringToString",
        }
    }
}

/// A flag's built-in default value.
pub enum DefaultVal {
    /// A compile-time constant default (the pflag `DefValue` string verbatim).
    Static(&'static str),
    /// A default resolved at runtime (sourced from `ava-snow` /
    /// `ava-network` constants, OS/cpu-count probes, …) so it cannot drift.
    Lazy(fn() -> String),
}

impl DefaultVal {
    /// Resolve the default to its pflag `DefValue` string form.
    #[must_use]
    pub fn resolve(&self) -> String {
        match self {
            Self::Static(s) => (*s).to_string(),
            Self::Lazy(f) => f(),
        }
    }
}

/// One row of the flag catalog (specs 12 §1.4).
pub struct FlagSpec {
    /// The exact Go flag string, e.g. `network-id` (see [`crate::keys`]).
    pub key: &'static str,
    /// The pflag value type.
    pub kind: FlagKind,
    /// The built-in default.
    pub default: DefaultVal,
    /// The Go help text, verbatim.
    pub help: &'static str,
    /// `Some(deprecation message)` if the key is deprecated
    /// (Go `pflag.MarkDeprecated`).
    pub deprecated: Option<&'static str>,
}

/// The full flag catalog: one row per Go flag registration, sorted by key
/// (specs 13 §1–§22; the committed Go snapshot is the drift guard).
pub static FLAG_SPECS: &[FlagSpec] = &[
    FlagSpec {
        key: keys::KEY_ACP_OBJECT,
        kind: FlagKind::IntSlice,
        default: DefaultVal::Static("[]"),
        help: "ACPs to object adoption",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_ACP_SUPPORT,
        kind: FlagKind::IntSlice,
        default: DefaultVal::Static("[]"),
        help: "ACPs to support adoption",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_API_ADMIN_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "If true, this node exposes the Admin API",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_API_HEALTH_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("true"),
        help: "If true, this node exposes the Health API",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_API_INFO_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("true"),
        help: "If true, this node exposes the Info API",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_API_METRICS_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("true"),
        help: "If true, this node exposes the Metrics API",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_BENCHLIST_BENCH_PROBABILITY,
        kind: FlagKind::F64,
        default: DefaultVal::Static("0.5"),
        help: "EWMA failure probability above which a node is benched",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_BENCHLIST_DURATION,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("5m0s"),
        help: "Max amount of time a peer is benchlisted",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_BENCHLIST_HALFLIFE,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1m0s"),
        help: "Halflife of the EWMA averager used for benchlisting",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_BENCHLIST_UNBENCH_PROBABILITY,
        kind: FlagKind::F64,
        default: DefaultVal::Static("0.2"),
        help: "EWMA failure probability below which a node is unbenched",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_BOOTSTRAP_ANCESTORS_MAX_CONTAINERS_RECEIVED,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("2000"),
        help: "This node reads at most this many containers from an incoming Ancestors message",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_BOOTSTRAP_ANCESTORS_MAX_CONTAINERS_SENT,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("2000"),
        help: "Max number of containers in an Ancestors message sent by this node",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_BOOTSTRAP_BEACON_CONNECTION_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1m0s"),
        help: "Timeout before emitting a warn log when connecting to bootstrapping beacons",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_BOOTSTRAP_IDS,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Comma separated list of bootstrap peer ids to connect to. Example: NodeID-JR4dVmy6ffUGAKCBDkyCbeZbyHQBeDsET,NodeID-8CrVPQZ4VSqgL8zTdvL14G8HqAfrBr4z",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_BOOTSTRAP_IPS,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Comma separated list of bootstrap peer ips to connect to. Example: 127.0.0.1:9630,127.0.0.1:9631",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_BOOTSTRAP_MAX_TIME_GET_ANCESTORS,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("50ms"),
        help: "Max Time to spend fetching a container and its ancestors when responding to a GetAncestors",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CHAIN_ALIASES_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/configs/chains/aliases.json"),
        help: "Specifies a JSON file that maps blockchainIDs with custom aliases. Ignored if chain-config-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CHAIN_ALIASES_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded map from blockchainID to custom aliases",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CHAIN_CONFIG_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded chains configurations",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CHAIN_CONFIG_DIR,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/configs/chains"),
        help: "Chain specific configurations parent directory. Ignored if chain-config-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CHAIN_DATA_DIR,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/chainData"),
        help: "Chain specific data directory",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CONFIG_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies a config file. Ignored if config-file-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CONFIG_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded config content",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CONFIG_FILE_CONTENT_TYPE,
        kind: FlagKind::String,
        default: DefaultVal::Static("json"),
        help: "Specifies the format of the base64 encoded config content. Available values: 'json', 'yaml', 'toml'",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CONSENSUS_APP_CONCURRENCY,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("2"),
        help: "Maximum number of goroutines to use when handling App messages on a chain",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CONSENSUS_FRONTIER_POLL_FREQUENCY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("100ms"),
        help: "Frequency of polling for new consensus frontiers",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CONSENSUS_SHUTDOWN_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1m0s"),
        help: "Timeout before killing an unresponsive chain",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_CREATE_ASSET_TX_FEE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_CREATE_ASSET_TX_FEE.to_string()),
        help: "Transaction fee, in nAVAX, for transactions that create new assets",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DATA_DIR,
        kind: FlagKind::String,
        default: DefaultVal::Static("$HOME/.avalanchego"),
        help: "Sets the base data directory where default sub-directories will be placed unless otherwise specified.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DB_CONFIG_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Path to database config file. Ignored if db-config-file-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DB_CONFIG_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded database config content",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DB_DIR,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/db"),
        help: "Path to database directory",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DB_READ_ONLY,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "If true, database writes are to memory and never persisted. May still initialize database directory/files on disk if they don't exist",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DB_TYPE,
        kind: FlagKind::String,
        default: DefaultVal::Static("leveldb"),
        help: "Database type to use. Must be one of {leveldb, memdb, pebbledb}",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DYNAMIC_FEES_BANDWIDTH_WEIGHT,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_DYNAMIC_FEES_BANDWIDTH_WEIGHT.to_string()),
        help: "Complexity multiplier used to convert Bandwidth into Gas",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DYNAMIC_FEES_COMPUTE_WEIGHT,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_DYNAMIC_FEES_COMPUTE_WEIGHT.to_string()),
        help: "Complexity multiplier used to convert Compute into Gas",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DYNAMIC_FEES_DB_READ_WEIGHT,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_DYNAMIC_FEES_DB_READ_WEIGHT.to_string()),
        help: "Complexity multiplier used to convert DB Reads into Gas",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DYNAMIC_FEES_DB_WRITE_WEIGHT,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_DYNAMIC_FEES_DB_WRITE_WEIGHT.to_string()),
        help: "Complexity multiplier used to convert DB Writes into Gas",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DYNAMIC_FEES_EXCESS_CONVERSION_CONSTANT,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| {
            defaults::LOCAL_DYNAMIC_FEES_EXCESS_CONVERSION_CONSTANT.to_string()
        }),
        help: "Constant to convert excess Gas to the Gas price",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DYNAMIC_FEES_MAX_GAS_CAPACITY,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_DYNAMIC_FEES_MAX_GAS_CAPACITY.to_string()),
        help: "Maximum amount of Gas the chain is allowed to store for future use",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DYNAMIC_FEES_MAX_GAS_PER_SECOND,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_DYNAMIC_FEES_MAX_GAS_PER_SECOND.to_string()),
        help: "Rate at which Gas is stored for future use",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DYNAMIC_FEES_MIN_GAS_PRICE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_DYNAMIC_FEES_MIN_GAS_PRICE.to_string()),
        help: "Minimum Gas price",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_DYNAMIC_FEES_TARGET_GAS_PER_SECOND,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| {
            defaults::LOCAL_DYNAMIC_FEES_TARGET_GAS_PER_SECOND.to_string()
        }),
        help: "Target rate of Gas usage",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_FD_LIMIT,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::FD_LIMIT_DEFAULT.to_string()),
        help: "Attempts to raise the process file descriptor limit to at least this value and error if the value is above the system max",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_GENESIS_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies a genesis config file path. Ignored when running standard networks or if genesis-file-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_GENESIS_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded genesis content",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HEALTH_CHECK_AVERAGER_HALFLIFE,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("10s"),
        help: "Halflife of averager when calculating a running average in a health check",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HEALTH_CHECK_FREQUENCY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("30s"),
        help: "Time between health checks",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_ALLOWED_HOSTS,
        kind: FlagKind::StringSlice,
        default: DefaultVal::Static("[localhost]"),
        help: "List of acceptable host names in API requests. Provide the wildcard ('*') to accept requests from all hosts. API requests where the Host field is empty or an IP address will always be accepted. An API call whose HTTP Host field isn't acceptable will receive a 403 error code",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_ALLOWED_ORIGINS,
        kind: FlagKind::String,
        default: DefaultVal::Static("*"),
        help: "Origins to allow on the HTTP port. Defaults to * which allows all origins. Example: https://*.avax.network https://*.avax-test.network",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_HOST,
        kind: FlagKind::String,
        default: DefaultVal::Static("127.0.0.1"),
        help: "Address of the HTTP server. If the address is empty or a literal unspecified IP address, the server will bind on all available unicast and anycast IP addresses of the local system",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_IDLE_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("2m0s"),
        help: "Maximum duration to wait for the next request when keep-alives are enabled. If http-idle-timeout is zero, the value of http-read-timeout is used. If both are zero, there is no timeout.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_PORT,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("9650"),
        help: "Port of the HTTP server. If the port is 0 a port number is automatically chosen",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_READ_HEADER_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("30s"),
        help: "Maximum duration to read request headers. The connection's read deadline is reset after reading the headers. If http-read-header-timeout is zero, the value of http-read-timeout is used. If both are zero, there is no timeout.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_READ_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("30s"),
        help: "Maximum duration for reading the entire request, including the body. A zero or negative value means there will be no timeout",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_SHUTDOWN_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("10s"),
        help: "Maximum duration to wait for existing connections to complete during node shutdown",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_SHUTDOWN_WAIT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("0s"),
        help: "Duration to wait after receiving SIGTERM or SIGINT before initiating shutdown. The /health endpoint will return unhealthy during this duration",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_TLS_CERT_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "TLS certificate file for the HTTPs server. Ignored if http-tls-cert-file-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_TLS_CERT_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded TLS certificate for the HTTPs server",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_TLS_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "Upgrade the HTTP server to HTTPs",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_TLS_KEY_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "TLS private key file for the HTTPs server. Ignored if http-tls-key-file-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_TLS_KEY_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded TLS private key for the HTTPs server",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_HTTP_WRITE_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("30s"),
        help: "Maximum duration before timing out writes of the response. It is reset whenever a new request's header is read. A zero or negative value means there will be no timeout.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_INDEX_ALLOW_INCOMPLETE,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "If true, allow running the node in such a way that could cause an index to miss transactions. Ignored if index is disabled",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_INDEX_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "If true, index all accepted containers and transactions and expose them via an API",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_LOG_DIR,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/logs"),
        help: "Logging directory for Avalanche",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_LOG_DISABLE_DISPLAY_PLUGIN_LOGS,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "Disables displaying plugin logs in stdout.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_LOG_DISPLAY_LEVEL,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "The log display level. If left blank, will inherit the value of log-level. Otherwise, should be one of {verbo, debug, trace, info, warn, error, fatal, off}",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_LOG_FORMAT,
        kind: FlagKind::String,
        default: DefaultVal::Static("auto"),
        help: "The structure of log format. Defaults to 'auto' which formats terminal-like logs, when the output is a terminal. Otherwise, should be one of {auto, plain, colors, json}",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_LOG_LEVEL,
        kind: FlagKind::String,
        default: DefaultVal::Static("info"),
        help: "The log level. Should be one of {verbo, debug, trace, info, warn, error, fatal, off}",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_LOG_ROTATER_COMPRESS_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "Enables the compression of rotated log files through gzip.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_LOG_ROTATER_MAX_AGE,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("0"),
        help: "The maximum number of days to retain old log files based on the timestamp encoded in their filename. 0 means retain all old log files.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_LOG_ROTATER_MAX_FILES,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("7"),
        help: "The maximum number of old log files to retain. 0 means retain all old log files.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_LOG_ROTATER_MAX_SIZE,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("8"),
        help: "The maximum file size in megabytes of the log file before it gets rotated.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_MAX_STAKE_DURATION,
        kind: FlagKind::Duration,
        default: DefaultVal::Lazy(|| format_go_duration(defaults::LOCAL_MAX_STAKE_DURATION)),
        help: "Maximum staking duration",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_MAX_VALIDATOR_STAKE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_MAX_VALIDATOR_STAKE.to_string()),
        help: "Maximum stake, in nAVAX, that can be placed on a validator on the primary network",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_METER_VMS_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("true"),
        help: "Enable Meter VMs to track VM performance with more granularity",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_MIN_DELEGATION_FEE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_MIN_DELEGATION_FEE.to_string()),
        help: "Minimum delegation fee, in the range [0, 1000000], that can be charged for delegation on the primary network",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_MIN_DELEGATOR_STAKE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_MIN_DELEGATOR_STAKE.to_string()),
        help: "Minimum stake, in nAVAX, that can be delegated on the primary network",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_MIN_STAKE_DURATION,
        kind: FlagKind::Duration,
        default: DefaultVal::Lazy(|| format_go_duration(defaults::LOCAL_MIN_STAKE_DURATION)),
        help: "Minimum staking duration",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_MIN_VALIDATOR_STAKE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_MIN_VALIDATOR_STAKE.to_string()),
        help: "Minimum stake, in nAVAX, required to validate the primary network",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_ALLOW_PRIVATE_IPS,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "Allows the node to initiate outbound connection attempts to peers with private IPs. If the provided --network-id is one of [mainnet, fuji] the default is false. Oterhwise, the default is true",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_COMPRESSION_TYPE,
        kind: FlagKind::String,
        default: DefaultVal::Static("zstd"),
        help: "Compression type for outbound messages. Must be one of [zstd, none]",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_HEALTH_MAX_OUTSTANDING_REQUEST_DURATION,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("5m0s"),
        help: "Node reports unhealthy if there has been a request outstanding for this duration",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_HEALTH_MAX_PORTION_SEND_QUEUE_FULL,
        kind: FlagKind::F64,
        default: DefaultVal::Static("0.9"),
        help: "Network layer returns unhealthy if more than this portion of the pending send queue is full",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_HEALTH_MAX_SEND_FAIL_RATE,
        kind: FlagKind::F64,
        default: DefaultVal::Static("0.9"),
        help: "Network layer reports unhealthy if more than this portion of attempted message sends fail",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_HEALTH_MAX_TIME_SINCE_MSG_RECEIVED,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1m0s"),
        help: "Network layer returns unhealthy if haven't received a message for at least this much time",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_HEALTH_MAX_TIME_SINCE_MSG_SENT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1m0s"),
        help: "Network layer returns unhealthy if haven't sent a message for at least this much time",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_HEALTH_MIN_CONN_PEERS,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("1"),
        help: "Network layer returns unhealthy if connected to less than this many peers",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_ID,
        kind: FlagKind::String,
        default: DefaultVal::Static("mainnet"),
        help: "Network ID this node will connect to",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_INBOUND_CONNECTION_THROTTLING_COOLDOWN,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("10s"),
        help: "Upgrade an inbound connection from a given IP at most once per this duration. If 0, don't rate-limit inbound connection upgrades",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_INBOUND_CONNECTION_THROTTLING_MAX_CONNS_PER_SEC,
        kind: FlagKind::F64,
        default: DefaultVal::Static("256"),
        help: "Max number of inbound connections to accept (from all peers) per second",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_INITIAL_RECONNECT_DELAY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1s"),
        help: "Initial delay duration must be waited before attempting to reconnect a peer",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_INITIAL_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("5s"),
        help: "Initial timeout value of the adaptive timeout manager",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_MAX_CLOCK_DIFFERENCE,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1m0s"),
        help: "Max allowed clock difference value between this node and peers",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_MAX_RECONNECT_DELAY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1m0s"),
        help: "Maximum delay duration must be waited before attempting to reconnect a peer",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_MAXIMUM_INBOUND_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("10s"),
        help: "Maximum timeout value of an inbound message. Defines duration within which an incoming message must be fulfilled. Incoming messages containing deadline higher than this value will be overridden with this value.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_MAXIMUM_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("10s"),
        help: "Maximum timeout value of the adaptive timeout manager",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_MINIMUM_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("2s"),
        help: "Minimum timeout value of the adaptive timeout manager",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_NO_INGRESS_CONNECTIONS_GRACE_PERIOD,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("10m0s"),
        help: "Time after which nodes are expected to be connected to us if we are a primary network validator, otherwise a health check fails",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_OUTBOUND_CONNECTION_THROTTLING_RPS,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("50"),
        help: "Make at most this number of outgoing peer connection attempts per second",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_OUTBOUND_CONNECTION_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("30s"),
        help: "Timeout when dialing a peer",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_PEER_LIST_BLOOM_RESET_FREQUENCY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1m0s"),
        help: "Frequency to recalculate the bloom filter used to request new peers from other nodes",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_PEER_LIST_NUM_VALIDATOR_IPS,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("15"),
        help: "Number of validator IPs to gossip to other nodes",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_PEER_LIST_PULL_GOSSIP_FREQUENCY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("2s"),
        help: "Frequency to request peers from other nodes",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_PEER_READ_BUFFER_SIZE,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("8192"),
        help: "Size, in bytes, of the buffer that we read peer messages into (there is one buffer per peer)",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_PEER_WRITE_BUFFER_SIZE,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("8192"),
        help: "Size, in bytes, of the buffer that we write peer messages into (there is one buffer per peer)",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_PING_FREQUENCY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("22.5s"),
        help: "Frequency of pinging other peers",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_PING_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("30s"),
        help: "Timeout value for Ping-Pong with a peer",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_READ_HANDSHAKE_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("15s"),
        help: "Timeout value for reading handshake messages",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_REQUIRE_VALIDATOR_TO_CONNECT,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "If true, this node will only maintain a connection with another node if this node is a validator, the other node is a validator, or the other node is a beacon",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_TCP_PROXY_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "Require all P2P connections to be initiated with a TCP proxy header",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_TCP_PROXY_READ_TIMEOUT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("3s"),
        help: "Maximum duration to wait for a TCP proxy header",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_TIMEOUT_COEFFICIENT,
        kind: FlagKind::F64,
        default: DefaultVal::Static("2"),
        help: "Multiplied by average network response time to get the network timeout. Must be >= 1",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_TIMEOUT_HALFLIFE,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("5m0s"),
        help: "Halflife of average network response time. Higher value --> network timeout is less volatile. Can't be 0",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_NETWORK_TLS_KEY_LOG_FILE_UNSAFE,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "TLS key log file path. Should only be specified for debugging",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PARTIAL_SYNC_PRIMARY_NETWORK,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "Only sync the P-chain on the Primary Network. If the node is a Primary Network validator, it will report unhealthy",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PLUGIN_DIR,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/plugins"),
        help: "Path to the plugin directory",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PROCESS_CONTEXT_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/process.json"),
        help: "The path to write process context to (including PID, API URI, and staking address).",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PROFILE_CONTINUOUS_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "Whether the app should continuously produce performance profiles",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PROFILE_CONTINUOUS_FREQ,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("15m0s"),
        help: "How frequently to rotate performance profiles",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PROFILE_CONTINUOUS_MAX_FILES,
        kind: FlagKind::I64,
        default: DefaultVal::Static("5"),
        help: "Maximum number of historical profiles to keep",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PROFILE_DIR,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/profiles"),
        help: "Path to the profile directory",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PROPOSERVM_MIN_BLOCK_DELAY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1s"),
        help: "Minimum delay to enforce when building a snowman++ block for the P-chain and X-chain",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PROPOSERVM_USE_CURRENT_HEIGHT,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "Have the ProposerVM always report the last accepted P-chain block height",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PUBLIC_IP,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Public IP of this node for P2P communication",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PUBLIC_IP_RESOLUTION_FREQUENCY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("5m0s"),
        help: "Frequency at which this node resolves/updates its public IP and renew NAT mappings, if applicable",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_PUBLIC_IP_RESOLUTION_SERVICE,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Only acceptable values are \"opendns\", \"ifconfigco\" or \"ifconfigme\". When provided, the node will use that service to periodically resolve/update its public IP",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_ROUTER_HEALTH_MAX_DROP_RATE,
        kind: FlagKind::F64,
        default: DefaultVal::Static("1"),
        help: "Node reports unhealthy if the router drops more than this portion of messages",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_ROUTER_HEALTH_MAX_OUTSTANDING_REQUESTS,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("1024"),
        help: "Node reports unhealthy if there are more than this many outstanding consensus requests (Get, PullQuery, etc.) over all chains",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SIMPLEX_MAX_NETWORK_DELAY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("5s"),
        help: "Maximum expected network delay for message transmission in Simplex consensus",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SIMPLEX_MAX_REBROADCAST_WAIT,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("5s"),
        help: "Time to retry message transmission in case of network instability",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SNOW_COMMIT_THRESHOLD,
        kind: FlagKind::I64,
        default: DefaultVal::Lazy(|| DEFAULT_PARAMETERS.beta.to_string()),
        help: "Beta value to use for consensus",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SNOW_CONCURRENT_REPOLLS,
        kind: FlagKind::I64,
        default: DefaultVal::Lazy(|| DEFAULT_PARAMETERS.concurrent_repolls.to_string()),
        help: "Minimum number of concurrent polls for finalizing consensus",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SNOW_CONFIDENCE_QUORUM_SIZE,
        kind: FlagKind::I64,
        default: DefaultVal::Lazy(|| DEFAULT_PARAMETERS.alpha_confidence.to_string()),
        help: "Threshold of nodes required to increase this node's confidence in a network poll. Ignored if snow-quorum-size is provided",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SNOW_MAX_PROCESSING,
        kind: FlagKind::I64,
        default: DefaultVal::Lazy(|| DEFAULT_PARAMETERS.max_outstanding_items.to_string()),
        help: "Maximum number of processing items to be considered healthy",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SNOW_MAX_TIME_PROCESSING,
        kind: FlagKind::Duration,
        default: DefaultVal::Lazy(|| {
            format_go_duration(DEFAULT_PARAMETERS.max_item_processing_time)
        }),
        help: "Maximum amount of time an item should be processing and still be healthy",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SNOW_OPTIMAL_PROCESSING,
        kind: FlagKind::I64,
        default: DefaultVal::Lazy(|| DEFAULT_PARAMETERS.optimal_processing.to_string()),
        help: "Optimal number of processing containers in consensus",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SNOW_PREFERENCE_QUORUM_SIZE,
        kind: FlagKind::I64,
        default: DefaultVal::Lazy(|| DEFAULT_PARAMETERS.alpha_preference.to_string()),
        help: "Threshold of nodes required to update this node's preference in a network poll. Ignored if snow-quorum-size is provided",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SNOW_QUORUM_SIZE,
        kind: FlagKind::I64,
        default: DefaultVal::Lazy(|| DEFAULT_PARAMETERS.alpha_confidence.to_string()),
        help: "Threshold of nodes required to update this node's preference and increase its confidence in a network poll",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SNOW_SAMPLE_SIZE,
        kind: FlagKind::I64,
        default: DefaultVal::Lazy(|| DEFAULT_PARAMETERS.k.to_string()),
        help: "Number of nodes to query for each network poll",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKE_MAX_CONSUMPTION_RATE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_STAKE_MAX_CONSUMPTION_RATE.to_string()),
        help: "Maximum consumption rate of the remaining tokens to mint in the staking function",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKE_MIN_CONSUMPTION_RATE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_STAKE_MIN_CONSUMPTION_RATE.to_string()),
        help: "Minimum consumption rate of the remaining tokens to mint in the staking function",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKE_MINTING_PERIOD,
        kind: FlagKind::Duration,
        default: DefaultVal::Lazy(|| format_go_duration(defaults::LOCAL_STAKE_MINTING_PERIOD)),
        help: "Consumption period of the staking function",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKE_SUPPLY_CAP,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_STAKE_SUPPLY_CAP.to_string()),
        help: "Supply cap of the staking function",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_EPHEMERAL_CERT_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "If true, the node uses an ephemeral staking TLS key and certificate, and has an ephemeral node ID",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_EPHEMERAL_SIGNER_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "If true, the node uses an ephemeral staking signer key",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_HOST,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Address of the consensus server. If the address is empty or a literal unspecified IP address, the server will bind on all available unicast and anycast IP addresses of the local system",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_PORT,
        kind: FlagKind::Uint,
        default: DefaultVal::Static("9651"),
        help: "Port of the consensus server. If the port is 0 a port number is automatically chosen",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_RPC_SIGNER_ENDPOINT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies the RPC endpoint of the staking signer",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_SIGNER_KEY_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/staking/signer.key"),
        help: "Path to the signer private key for staking. Ignored if staking-signer-key-file-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_SIGNER_KEY_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded signer private key for staking",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_TLS_CERT_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/staking/staker.crt"),
        help: "Path to the TLS certificate for staking. Ignored if staking-tls-cert-file-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_TLS_CERT_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded TLS certificate for staking",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_TLS_KEY_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/staking/staker.key"),
        help: "Path to the TLS private key for staking. Ignored if staking-tls-key-file-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STAKING_TLS_KEY_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded TLS private key for staking",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STATE_SYNC_IDS,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Comma separated list of state sync peer ids to connect to. Example: NodeID-JR4dVmy6ffUGAKCBDkyCbeZbyHQBeDsET,NodeID-8CrVPQZ4VSqgL8zTdvL14G8HqAfrBr4z",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_STATE_SYNC_IPS,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Comma separated list of state sync peer ips to connect to. Example: 127.0.0.1:9630,127.0.0.1:9631",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SUBNET_CONFIG_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded subnets configurations",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SUBNET_CONFIG_DIR,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/configs/subnets"),
        help: "Subnet specific configurations parent directory. Ignored if subnet-config-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SYBIL_PROTECTION_DISABLED_WEIGHT,
        kind: FlagKind::U64,
        default: DefaultVal::Static("100"),
        help: "Weight to provide to each peer when sybil protection is disabled",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SYBIL_PROTECTION_ENABLED,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("true"),
        help: "Enables sybil protection. If enabled, Network TLS is required",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SYSTEM_TRACKER_CPU_HALFLIFE,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("15s"),
        help: "Halflife to use for the cpu tracker. Larger halflife --> cpu usage metrics change more slowly",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SYSTEM_TRACKER_DISK_HALFLIFE,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("1m0s"),
        help: "Halflife to use for the disk tracker. Larger halflife --> disk usage metrics change more slowly",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SYSTEM_TRACKER_DISK_REQUIRED_AVAILABLE_SPACE,
        kind: FlagKind::U64,
        default: DefaultVal::Static("0"),
        help: "DEPRECATED: Minimum number of available bytes on disk, under which the node will shutdown.",
        deprecated: Some("Use system-tracker-disk-required-available-space-percentage instead"),
    },
    FlagSpec {
        key: keys::KEY_SYSTEM_TRACKER_DISK_REQUIRED_AVAILABLE_SPACE_PERCENTAGE,
        kind: FlagKind::U64,
        default: DefaultVal::Static("3"),
        help: "Minimum percentage (between 0 and 50) of available disk space, under which the node will shutdown.",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SYSTEM_TRACKER_DISK_WARNING_AVAILABLE_SPACE_PERCENTAGE,
        kind: FlagKind::U64,
        default: DefaultVal::Static("10"),
        help: "Warning threshold for the percentage (between 0 and 50) of available disk space, under which the node will be considered unhealthy. Must be >= [system-tracker-disk-required-available-space-percentage]",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SYSTEM_TRACKER_DISK_WARNING_THRESHOLD_AVAILABLE_SPACE,
        kind: FlagKind::U64,
        default: DefaultVal::Static("0"),
        help: "DEPRECATED: Warning threshold for the number of available bytes on disk, under which the node will be considered unhealthy.  Must be >= [system-tracker-disk-required-available-space]",
        deprecated: Some("Use system-tracker-disk-warning-available-space-percentage instead"),
    },
    FlagSpec {
        key: keys::KEY_SYSTEM_TRACKER_FREQUENCY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("500ms"),
        help: "Frequency to check the real system usage of tracked processes. More frequent checks --> usage metrics are more accurate, but more expensive to track",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_SYSTEM_TRACKER_PROCESSING_HALFLIFE,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("15s"),
        help: "Halflife to use for the processing requests tracker. Larger halflife --> usage metrics change more slowly",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_AT_LARGE_ALLOC_SIZE,
        kind: FlagKind::U64,
        default: DefaultVal::Static("6291456"),
        help: "Size, in bytes, of at-large byte allocation in inbound message throttler",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_BANDWIDTH_MAX_BURST_SIZE,
        kind: FlagKind::U64,
        default: DefaultVal::Static("2097152"),
        help: "Max inbound bandwidth a node can use at once. Must be at least the max message size. See BandwidthThrottler",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_BANDWIDTH_REFILL_RATE,
        kind: FlagKind::U64,
        default: DefaultVal::Static("524288"),
        help: "Max average inbound bandwidth usage of a peer, in bytes per second. See BandwidthThrottler",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_CPU_MAX_NON_VALIDATOR_NODE_USAGE,
        kind: FlagKind::F64,
        default: DefaultVal::Lazy(defaults::cpu_max_non_validator_node_usage_default),
        help: "Maximum number of CPUs that a non-validator can utilize. Value should be in range [0, total core count]",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_CPU_MAX_NON_VALIDATOR_USAGE,
        kind: FlagKind::F64,
        default: DefaultVal::Lazy(defaults::cpu_max_non_validator_usage_default),
        help: "Number of CPUs that if fully utilized, will rate limit all non-validators. Value should be in range [0, total core count]",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_CPU_MAX_RECHECK_DELAY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("5s"),
        help: "In the CPU-based network throttler, check at least this often whether the node's CPU usage has fallen to an acceptable level",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_CPU_VALIDATOR_ALLOC,
        kind: FlagKind::F64,
        default: DefaultVal::Lazy(defaults::cpu_validator_alloc_default),
        help: "Maximum number of CPUs to allocate for use by validators. Value should be in range [0, total core count]",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_DISK_MAX_NON_VALIDATOR_NODE_USAGE,
        kind: FlagKind::F64,
        default: DefaultVal::Static("1.073741824e+12"),
        help: "Maximum number of disk reads/writes per second that a non-validator can utilize. Must be >= 0",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_DISK_MAX_NON_VALIDATOR_USAGE,
        kind: FlagKind::F64,
        default: DefaultVal::Static("1.073741824e+12"),
        help: "Number of disk reads/writes per second that, if fully utilized, will rate limit all non-validators. Must be >= 0",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_DISK_MAX_RECHECK_DELAY,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("5s"),
        help: "In the disk-based network throttler, check at least this often whether the node's disk usage has fallen to an acceptable level",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_DISK_VALIDATOR_ALLOC,
        kind: FlagKind::F64,
        default: DefaultVal::Static("1.073741824e+12"),
        help: "Maximum number of disk reads/writes per second to allocate for use by validators. Must be > 0",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_NODE_MAX_AT_LARGE_BYTES,
        kind: FlagKind::U64,
        default: DefaultVal::Static("2097152"),
        help: "Max number of bytes a node can take from the inbound message throttler's at-large allocation. Must be at least the max message size",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_NODE_MAX_PROCESSING_MSGS,
        kind: FlagKind::U64,
        default: DefaultVal::Static("1024"),
        help: "Max number of messages currently processing from a given node",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_INBOUND_VALIDATOR_ALLOC_SIZE,
        kind: FlagKind::U64,
        default: DefaultVal::Static("33554432"),
        help: "Size, in bytes, of validator byte allocation in inbound message throttler",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_OUTBOUND_AT_LARGE_ALLOC_SIZE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| DEFAULT_AT_LARGE_ALLOC_SIZE.to_string()),
        help: "Size, in bytes, of at-large byte allocation in outbound message throttler",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_OUTBOUND_NODE_MAX_AT_LARGE_BYTES,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| DEFAULT_NODE_MAX_AT_LARGE_BYTES.to_string()),
        help: "Max number of bytes a node can take from the outbound message throttler's at-large allocation. Must be at least the max message size",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_THROTTLER_OUTBOUND_VALIDATOR_ALLOC_SIZE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| DEFAULT_VDR_ALLOC_SIZE.to_string()),
        help: "Size, in bytes, of validator byte allocation in outbound message throttler",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_TRACING_ENDPOINT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "The endpoint to send trace data to. If unspecified, the default endpoint will be used; depending on the exporter type",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_TRACING_EXPORTER_TYPE,
        kind: FlagKind::String,
        default: DefaultVal::Static("disabled"),
        help: "Type of exporter to use for tracing. Options are [disabled, grpc, http]",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_TRACING_HEADERS,
        kind: FlagKind::StringMap,
        default: DefaultVal::Static("[]"),
        help: "The headers to provide the trace indexer",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_TRACING_INSECURE,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("true"),
        help: "If true, don't use TLS when sending trace data",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_TRACING_SAMPLE_RATE,
        kind: FlagKind::F64,
        default: DefaultVal::Static("0.1"),
        help: "The fraction of traces to sample. If >= 1, always sample. If <= 0, never sample",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_TRACK_SUBNETS,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "List of subnets for the node to track. A node tracking a subnet will track the uptimes of the subnet validators and attempt to sync all the chains in the subnet. Before validating a subnet, a node should be tracking the subnet to avoid impacting their subnet validation uptime",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_TX_FEE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_TX_FEE.to_string()),
        help: "Transaction fee, in nAVAX",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_UPGRADE_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies an upgrade config file path. Ignored when running standard networks or if upgrade-file-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_UPGRADE_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded upgrade content",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_UPTIME_METRIC_FREQ,
        kind: FlagKind::Duration,
        default: DefaultVal::Static("30s"),
        help: "Frequency of renewing this node's average uptime metric",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_UPTIME_REQUIREMENT,
        kind: FlagKind::F64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_UPTIME_REQUIREMENT.to_string()),
        help: "Fraction of time a validator must be online to receive rewards",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_VALIDATOR_FEES_CAPACITY,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_VALIDATOR_FEES_CAPACITY.to_string()),
        help: "Maximum number of validators",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_VALIDATOR_FEES_EXCESS_CONVERSION_CONSTANT,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| {
            defaults::LOCAL_VALIDATOR_FEES_EXCESS_CONVERSION_CONSTANT.to_string()
        }),
        help: "Constant to convert validator excess price",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_VALIDATOR_FEES_MIN_PRICE,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_VALIDATOR_FEES_MIN_PRICE.to_string()),
        help: "Minimum validator price in nAVAX per second",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_VALIDATOR_FEES_TARGET,
        kind: FlagKind::U64,
        default: DefaultVal::Lazy(|| defaults::LOCAL_VALIDATOR_FEES_TARGET.to_string()),
        help: "Target number of validators",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_VERSION,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "If true, print version and quit",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_VERSION_JSON,
        kind: FlagKind::Bool,
        default: DefaultVal::Static("false"),
        help: "If true, print version in JSON format and quit",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_VM_ALIASES_FILE,
        kind: FlagKind::String,
        default: DefaultVal::Static("$AVALANCHEGO_DATA_DIR/configs/vms/aliases.json"),
        help: "Specifies a JSON file that maps vmIDs with custom aliases. Ignored if vm-aliases-file-content is specified",
        deprecated: None,
    },
    FlagSpec {
        key: keys::KEY_VM_ALIASES_FILE_CONTENT,
        kind: FlagKind::String,
        default: DefaultVal::Static(""),
        help: "Specifies base64 encoded maps vmIDs with custom aliases",
        deprecated: None,
    },
];

/// Adapts [`parse_go_duration`] to clap's value-parser signature.
fn clap_go_duration(s: &str) -> Result<std::time::Duration, crate::ConfigError> {
    parse_go_duration(s)
}

/// Builds the `avalanchers` [`clap::Command`] programmatically from the flag
/// table, so flag names stay data the parity test can enumerate (12 §1.4).
///
/// pflag-parity choices:
/// - Bools take `--x` and `--x=true|false` (`num_args(0..=1)` +
///   `default_missing_value("true")`).
/// - Durations parse with Go's `time.ParseDuration` grammar (not humantime).
/// - Slices split on commas and may repeat; `stringToString` repeats `k=v`.
/// - Deprecated flags get a `DEPRECATED:` help prefix (Go prints
///   `Flag --<key> has been deprecated, <msg>`).
/// - clap's auto `--version` is disabled: `version`/`version-json` are real
///   table rows handled by the binary (12 §9).
#[must_use]
pub fn build_command(specs: &'static [FlagSpec]) -> Command {
    let mut cmd = Command::new("avalanchers")
        .version(ava_version::CURRENT.to_string())
        .disable_version_flag(true)
        .disable_help_flag(false)
        .arg_required_else_help(false);
    for s in specs {
        let mut arg = Arg::new(s.key).long(s.key);
        arg = match s.kind {
            FlagKind::Bool => arg
                .num_args(0..=1)
                .default_missing_value("true")
                .value_parser(clap::value_parser!(bool)),
            FlagKind::Duration => arg.value_parser(clap_go_duration),
            FlagKind::StringSlice | FlagKind::IntSlice => {
                arg.value_delimiter(',').action(ArgAction::Append)
            }
            FlagKind::StringMap => arg.action(ArgAction::Append),
            FlagKind::String | FlagKind::U64 | FlagKind::Uint | FlagKind::I64 | FlagKind::F64 => {
                arg
            }
        };
        arg = match s.deprecated {
            Some(msg) => arg.help(format!("DEPRECATED: {msg}")),
            None => arg.help(s.help),
        };
        cmd = cmd.arg(arg);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn every_key_has_one_spec() {
        // 13 §24: 206 keys, no orphans, no dupes.
        assert_eq!(FLAG_SPECS.len(), 206);
        let spec_keys: HashSet<&str> = FLAG_SPECS.iter().map(|s| s.key).collect();
        assert_eq!(spec_keys.len(), 206, "duplicate keys in FLAG_SPECS");
        let all_keys: HashSet<&str> = crate::keys::ALL_KEYS.iter().copied().collect();
        assert_eq!(spec_keys, all_keys);
        // Sorted by key, like the Go snapshot (13 §25).
        assert!(FLAG_SPECS.is_sorted_by(|a, b| a.key < b.key));
    }

    #[test]
    fn build_command_accepts_bool_forms() {
        // pflag bools accept both `--x` and `--x=true|false` (12 §1.4).
        for (args, want) in [
            (vec!["avalanchers", "--sybil-protection-enabled"], true),
            (vec!["avalanchers", "--sybil-protection-enabled=true"], true),
            (
                vec!["avalanchers", "--sybil-protection-enabled=false"],
                false,
            ),
        ] {
            let m = build_command(FLAG_SPECS)
                .try_get_matches_from(args.clone())
                .unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert_eq!(
                m.get_one::<bool>(keys::KEY_SYBIL_PROTECTION_ENABLED),
                Some(&want),
                "{args:?}"
            );
        }
    }

    #[test]
    fn build_command_parses_durations_and_slices() {
        let m = build_command(FLAG_SPECS)
            .try_get_matches_from([
                "avalanchers",
                "--network-ping-timeout=22.5s",
                "--http-allowed-hosts=a.example,b.example",
                "--acp-support=1,2",
            ])
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            m.get_one::<std::time::Duration>(keys::KEY_NETWORK_PING_TIMEOUT),
            Some(&std::time::Duration::from_millis(22_500))
        );
        let hosts: Vec<&String> = m
            .get_many::<String>(keys::KEY_HTTP_ALLOWED_HOSTS)
            .map(Iterator::collect)
            .unwrap_or_default();
        assert_eq!(hosts, ["a.example", "b.example"]);
        let acps: Vec<&String> = m
            .get_many::<String>(keys::KEY_ACP_SUPPORT)
            .map(Iterator::collect)
            .unwrap_or_default();
        assert_eq!(acps, ["1", "2"]);
    }

    #[test]
    fn flag_kind_maps_to_go_type_string() {
        // The 10 pflag type strings in specs 13 §25.
        let want = [
            (FlagKind::Bool, "bool"),
            (FlagKind::String, "string"),
            (FlagKind::U64, "uint64"),
            (FlagKind::Uint, "uint"),
            (FlagKind::I64, "int"),
            (FlagKind::F64, "float64"),
            (FlagKind::Duration, "duration"),
            (FlagKind::StringSlice, "stringSlice"),
            (FlagKind::IntSlice, "intSlice"),
            (FlagKind::StringMap, "stringToString"),
        ];
        for (kind, s) in want {
            assert_eq!(kind.go_type_str(), s, "{kind:?}");
        }
    }
}
