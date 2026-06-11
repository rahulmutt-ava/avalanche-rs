# `ava-config` — Go → Rust porting matrix

Tracks coverage of Go `config/...` tests (specs 02 §13) against the
`../avalanchego` reference tree, plus the M8 golden/property exit gates from
`plan/M8-node-config-api.md`. Rows are seeded from `go test -list '.*'
./config/...`.

Legend: ⬜ not ported · 🟡 partial · ✅ ported · n/a not applicable

## Golden snapshot regeneration (specs 13 §25)

```sh
# Drops the embedded emitter into $AVALANCHEGO_DIR/config/ (default
# ../avalanchego), runs it env-gated, rewrites the snapshot, deletes the
# emitter. Requires Go 1.25.x on PATH.
AVALANCHEGO_DIR=../avalanchego cargo xtask gen-flags
```

Snapshot: `tests/vectors/config/flags.json` — sorted
`{name,type,default,deprecated,deprecation_msg}` records plus a `_provenance`
header (source Go commit + the pinned symbolic-default rule).

**Symbolic-default normalization:** host-dependent pflag `DefValue`s are pinned
to symbolic strings on BOTH sides so the snapshot is host-independent:
`fd-limit` → `DefaultFDLimit` (32768 linux/bsd, 10240 darwin),
`throttler-inbound-cpu-validator-alloc` → `NumCPU`,
`throttler-inbound-cpu-max-non-validator-usage` → `0.8*NumCPU`,
`throttler-inbound-cpu-max-non-validator-node-usage` → `NumCPU/8`.
Duration defaults are compared after a `parse_go_duration` →
`format_go_duration` round-trip (`"5m"` ≡ `"5m0s"`).

---

### config (flags, keys, viper precedence)

| Go test | Status | Rust counterpart / note |
|---|---|---|
| `BuildFlagSet` registration set (no single Go unit test; guarded by usage) | ✅ ported | `golden_flag_parity.rs::flag_parity` — set-equality of names + per-flag type/default/deprecation vs the committed Go snapshot (M8.4 exit gate) |
| `keys.go` ↔ `flags.go` join (13 §24 audit) | ✅ ported | `keys::tests::key_count_matches_go`, `keys::tests::keys_are_sorted_and_unique`, `flags::tests::every_key_has_one_spec` |
| pflag bool grammar (`--x` / `--x=true`) | ✅ ported | `flags::tests::build_command_accepts_bool_forms` |
| pflag slice/duration value grammar | ✅ ported | `flags::tests::build_command_parses_durations_and_slices`, `duration::tests::parse_go_duration_grammar`, `duration::tests::parse_go_duration_errors_match_go` (Go `time.ParseDuration` error shapes) |
| `TestGetEnvVarName` (`config/viper.go`) | ✅ ported | `precedence::tests::env_var_name_mapping` (M8.9) |
| viper precedence (flag > env > file > default) + `IsSet` | ✅ ported | `prop_config_precedence.rs::config_precedence` proptest + `precedence::tests::is_set_layers` + `precedence::tests::typed_getters_walk_all_layers` (M8.10/M8.11 exit gate); the `snow-quorum-size` override + `network-allow-private-ips` parse-time deferral pinned in `prop_config_precedence.rs` unit cases |
| config-file `-content` overrides path (json/yaml/toml) | ✅ ported | `precedence::tests::config_file_content_overrides_path`, `precedence::tests::config_file_content_types_funnel_to_json_value`, `precedence::tests::config_file_via_env_var`, `prop_config_precedence.rs::content_overrides_file` (M8.9) |
| `getExpandedArg` path expansion | ✅ ported | `precedence::tests::data_dir_expansion` (M8.10; `$AVALANCHEGO_DATA_DIR`, `$VAR`, `${VAR}`, unknown→`""`) |
| `TestGetChainConfigsFromFiles` / `...FromFlags` / dir-load family | ⬜ not ported | chain/subnet config-dir loaders are M8.13 |
| `TestGetVMAliases*` / `TestGetChainAliases*` | ⬜ not ported | alias-file loaders are M8.13 |
| `TestSubnetConfigs*` (`subnets.Config` defaulting/validation) | ⬜ not ported | M8.13 (`subnets.rs`) |
| `TestGetNodeConfig` derived/network-dependent defaults | ⬜ not ported | M8.12 (`parse.rs::get_node_config`) — incl. `network-allow-private-ips` effective default, bootstrap sampling, fee/staking ignore-on-standard-networks |

**Summary:** 8 ported ✅ / 0 partial 🟡 / 4 not ported ⬜ (owned by M8.12–M8.13) / 0 n/a.
