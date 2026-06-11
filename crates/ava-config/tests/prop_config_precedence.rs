// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `prop::config_precedence` — the M8.11 exit gate (specs 13 §25 step 4,
//! 02 §4): for every {CLI?, env?, file?} presence combination over sampled
//! keys of every flag kind, the resolved value comes from the highest present
//! layer (`CLI > env > file > default`) and `is_set` is true iff any
//! non-default layer is present.

#![allow(unused_crate_dependencies, clippy::unwrap_used, clippy::expect_used)]

use base64::Engine as _;
use proptest::prelude::*;

use ava_config::duration::format_go_duration;
use ava_config::flags::{FLAG_SPECS, build_command};
use ava_config::keys;
use ava_config::precedence::Layered;

/// One sampled flag: per-layer raw inputs + the canonical resolved string
/// expected from each layer.
struct Sample {
    key: &'static str,
    /// CLI argument (`--key=value`).
    cli_arg: &'static str,
    /// Env-var value (`AVAGO_*`).
    env_val: &'static str,
    /// JSON value for the config-file layer.
    file_json: serde_json::Value,
    /// Expected canonical resolution per layer: [cli, env, file, default].
    expect: [&'static str; 4],
    /// Canonicalizing getter (typed getter → comparable string).
    resolve: fn(&Layered, &str) -> String,
}

fn samples() -> Vec<Sample> {
    vec![
        // uint
        Sample {
            key: keys::KEY_HTTP_PORT,
            cli_arg: "--http-port=1001",
            env_val: "1002",
            file_json: serde_json::json!(1003),
            expect: ["1001", "1002", "1003", "9650"],
            resolve: |l, k| l.get_u64(k).expect("u64").to_string(),
        },
        // uint64 (Lazy LocalParams default)
        Sample {
            key: keys::KEY_TX_FEE,
            cli_arg: "--tx-fee=11",
            env_val: "12",
            file_json: serde_json::json!(13),
            expect: ["11", "12", "13", "1000000"],
            resolve: |l, k| l.get_u64(k).expect("u64").to_string(),
        },
        // int (Lazy snowball default)
        Sample {
            key: keys::KEY_SNOW_MAX_PROCESSING,
            cli_arg: "--snow-max-processing=21",
            env_val: "22",
            file_json: serde_json::json!(23),
            expect: ["21", "22", "23", "256"],
            resolve: |l, k| l.get_i64(k).expect("i64").to_string(),
        },
        // float64
        Sample {
            key: keys::KEY_TRACING_SAMPLE_RATE,
            cli_arg: "--tracing-sample-rate=0.25",
            env_val: "0.5",
            file_json: serde_json::json!(0.75),
            expect: ["0.25", "0.5", "0.75", "0.1"],
            resolve: |l, k| l.get_f64(k).expect("f64").to_string(),
        },
        // string
        Sample {
            key: keys::KEY_LOG_LEVEL,
            cli_arg: "--log-level=verbo",
            env_val: "debug",
            file_json: serde_json::json!("trace"),
            expect: ["verbo", "debug", "trace", "info"],
            resolve: |l, k| l.get_string(k).expect("string"),
        },
        // bool (expectation per layer, values alternate)
        Sample {
            key: keys::KEY_SYBIL_PROTECTION_ENABLED,
            cli_arg: "--sybil-protection-enabled=false",
            env_val: "true",
            file_json: serde_json::json!(false),
            expect: ["false", "true", "false", "true"],
            resolve: |l, k| l.get_bool(k).expect("bool").to_string(),
        },
        // duration
        Sample {
            key: keys::KEY_BENCHLIST_DURATION,
            cli_arg: "--benchlist-duration=1s",
            env_val: "2s",
            file_json: serde_json::json!("3s"),
            expect: ["1s", "2s", "3s", "5m0s"],
            resolve: |l, k| format_go_duration(l.get_duration(k).expect("duration")),
        },
        // stringSlice
        Sample {
            key: keys::KEY_HTTP_ALLOWED_HOSTS,
            cli_arg: "--http-allowed-hosts=a,b",
            env_val: "c,d",
            file_json: serde_json::json!(["e", "f"]),
            expect: ["a,b", "c,d", "e,f", "localhost"],
            resolve: |l, k| l.get_string_slice(k).expect("slice").join(","),
        },
        // intSlice
        Sample {
            key: keys::KEY_ACP_SUPPORT,
            cli_arg: "--acp-support=7,8",
            env_val: "9,10",
            file_json: serde_json::json!([11, 12]),
            expect: ["7,8", "9,10", "11,12", ""],
            resolve: |l, k| {
                l.get_int_slice(k)
                    .expect("ints")
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            },
        },
        // stringToString
        Sample {
            key: keys::KEY_TRACING_HEADERS,
            cli_arg: "--tracing-headers=k=v",
            env_val: "k=e",
            file_json: serde_json::json!({"k": "f"}),
            expect: ["k=v", "k=e", "k=f", ""],
            resolve: |l, k| {
                let mut pairs: Vec<String> = l
                    .get_string_map(k)
                    .expect("map")
                    .into_iter()
                    .map(|(key, val)| format!("{key}={val}"))
                    .collect();
                pairs.sort();
                pairs.join(",")
            },
        },
    ]
}

/// Builds a `Layered` with the sampled key present on the requested layers.
fn build_layered(sample: &Sample, on_cli: bool, in_env: bool, in_file: bool) -> Layered {
    let mut args = vec!["avalanchers".to_string()];
    if on_cli {
        args.push(sample.cli_arg.to_string());
    }
    if in_file {
        let body = serde_json::json!({ sample.key: sample.file_json }).to_string();
        let b64 = base64::engine::general_purpose::STANDARD.encode(body);
        args.push(format!("--config-file-content={b64}"));
    }
    let mut vars: Vec<(String, String)> = Vec::new();
    if in_env {
        let var = ava_config::precedence::env_var_name(sample.key);
        vars.push((var, sample.env_val.to_string()));
    }
    Layered::build_with_env(
        build_command(FLAG_SPECS),
        args,
        FLAG_SPECS,
        vars.into_iter(),
    )
    .expect("build layered")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn config_precedence(
        key_idx in 0usize..10,
        on_cli in any::<bool>(),
        in_env in any::<bool>(),
        in_file in any::<bool>(),
    ) {
        let all = samples();
        let sample = all.get(key_idx).expect("sample index");
        let layered = build_layered(sample, on_cli, in_env, in_file);

        // Highest present layer wins (CLI > env > file > default).
        let want = if on_cli {
            sample.expect[0]
        } else if in_env {
            sample.expect[1]
        } else if in_file {
            sample.expect[2]
        } else {
            sample.expect[3]
        };
        let got = (sample.resolve)(&layered, sample.key);
        prop_assert_eq!(got, want, "layers: cli={} env={} file={}", on_cli, in_env, in_file);

        // is_set ⟺ any non-default layer present (13 §23).
        prop_assert_eq!(layered.is_set(sample.key), on_cli || in_env || in_file);
    }
}

/// 13 §7/§23: when `--snow-quorum-size` is set, it overrides BOTH
/// `snow-preference-quorum-size` and `snow-confidence-quorum-size`. The
/// override itself happens in `getPrimaryNetworkSnowConfig` (M8.12); the
/// resolver contract it depends on is `is_set` + the getters, mirrored here.
#[test]
fn snow_quorum_size_is_set_drives_alpha_override() {
    let resolve_alphas = |layered: &Layered| -> (i64, i64) {
        if layered.is_set(keys::KEY_SNOW_QUORUM_SIZE) {
            let q = layered.get_i64(keys::KEY_SNOW_QUORUM_SIZE).expect("quorum");
            (q, q)
        } else {
            (
                layered
                    .get_i64(keys::KEY_SNOW_PREFERENCE_QUORUM_SIZE)
                    .expect("pref"),
                layered
                    .get_i64(keys::KEY_SNOW_CONFIDENCE_QUORUM_SIZE)
                    .expect("conf"),
            )
        }
    };

    // quorum-size set → both alphas take it, even with explicit pref/conf.
    let layered = Layered::build_with_env(
        build_command(FLAG_SPECS),
        [
            "avalanchers".to_string(),
            "--snow-quorum-size=17".to_string(),
            "--snow-preference-quorum-size=13".to_string(),
            "--snow-confidence-quorum-size=14".to_string(),
        ],
        FLAG_SPECS,
        std::iter::empty(),
    )
    .expect("build");
    assert_eq!(resolve_alphas(&layered), (17, 17));

    // quorum-size unset → the dedicated flags win.
    let layered = Layered::build_with_env(
        build_command(FLAG_SPECS),
        [
            "avalanchers".to_string(),
            "--snow-preference-quorum-size=13".to_string(),
            "--snow-confidence-quorum-size=14".to_string(),
        ],
        FLAG_SPECS,
        std::iter::empty(),
    )
    .expect("build");
    assert_eq!(resolve_alphas(&layered), (13, 14));
}

/// 13 §23: the base64 `-content` form beats the `-file` path form.
#[test]
fn content_overrides_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("conf.json");
    std::fs::write(&path, r#"{"http-port": 1111}"#).expect("write");
    let b64 = base64::engine::general_purpose::STANDARD.encode(r#"{"http-port": 2222}"#);
    let layered = Layered::build_with_env(
        build_command(FLAG_SPECS),
        [
            "avalanchers".to_string(),
            format!("--config-file={}", path.display()),
            format!("--config-file-content={b64}"),
        ],
        FLAG_SPECS,
        std::iter::empty(),
    )
    .expect("build");
    assert_eq!(layered.get_u64(keys::KEY_HTTP_PORT).expect("port"), 2222);
}

/// 13 §8: `network-allow-private-ips`' network-dependent EFFECTIVE default is
/// resolved at parse time (M8.12), NOT by the resolver — here the registered
/// pflag default (`false`) is returned and `is_set` stays false.
#[test]
fn network_allow_private_ips_not_resolved_here() {
    let layered = Layered::build_with_env(
        build_command(FLAG_SPECS),
        ["avalanchers".to_string(), "--network-id=local".to_string()],
        FLAG_SPECS,
        std::iter::empty(),
    )
    .expect("build");
    assert!(!layered.is_set(keys::KEY_NETWORK_ALLOW_PRIVATE_IPS));
    assert!(
        !layered
            .get_bool(keys::KEY_NETWORK_ALLOW_PRIVATE_IPS)
            .expect("bool")
    );
}
