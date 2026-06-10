// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! The flag-table model (specs 12 §1.4, 13 §25).
//!
//! Flags are declared as data ([`FlagSpec`]) so the `golden::flag_parity` test
//! can enumerate them and diff the generated set against the Go
//! `config.BuildFlagSet()` snapshot.

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

#[cfg(test)]
mod tests {
    use super::*;

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
