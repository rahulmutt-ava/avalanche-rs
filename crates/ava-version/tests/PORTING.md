# PORTING.md — `ava-version`

Parity against avalanchego `version/` and `upgrade/`. One row per upstream Go
test; status `todo` / `wip` / `ported` / `na`. No `wip` rows at the M0.25 exit
gate.

Owning tasks: M0.22 (Application + Compatibility), M0.23 (UpgradeConfig + Fork +
activation schedule), M0.24 (proptest suite), M9.22 (version-string /
compatibility-matrix interop conformance — golden legs).

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `version/application_test.go` | `tests/version.rs` | ported |
| `version/compatibility_test.go` | `tests/version.rs`, `tests/compat_matrix.rs` (`golden::compatibility_matrix`) | ported |
| `upgrade/upgrade_test.go` | `tests/golden_upgrade.rs` | ported |
| `version/compatibility.json` byte parity (`specs/26` §9(6)) | `tests/compat_matrix.rs` (`golden_json::compatibility_json_byte_parity`) | ported |
| `version` String/Semantic + `api/info` getNodeVersion fields (`specs/26` §9(1)/(2)) | `tests/compat_matrix.rs` (`golden_reply::node_version_reply`) | ported |
| _(proptest invariants — no direct Go counterpart; `specs/02` §4)_ | `tests/proptests.rs` | ported |

## M9.22 golden coverage (`specs/26` §9)

`tests/compat_matrix.rs` adds the three pure-Rust golden legs of M9.22:

- `golden::compatibility_matrix` — every mandatory `compatible()` cell from
  `specs/26` §9(3): newer-major reject (clause 1); below-pre-floor reject;
  at/above-pre-floor accept (clock < upgrade_time); fork-boundary cut-over
  (peer in `[pre-floor, post-floor)` rejected once clock ≥ upgrade_time);
  peer == current accept; newer same-major accept (log-only); different-`name`
  compatible-triple accept; and the mid-connection transition.
- `golden::compatibility_json_byte_parity` — `crates/ava-version/compatibility.json`
  is committed byte-identical to the Go tree's `version/compatibility.json`
  (provenance in `compatibility.json.md`; upstream commit `0b0b57143c`) and
  parses to the same table the code loads (`rpc_chain_vm_protocol_compatibility()`).
- `golden::node_version_reply` — version-string display goldens and the
  `info.getNodeVersion` fields `ava-version` owns (`version`, `databaseVersion`,
  `rpcProtocolVersion` as the Go `json.Uint32` STRING form). The FULL reply JSON
  (incl. `gitCommit`, `vmVersions`) is golden-tested at the `ava-api` layer
  (`crates/ava-api/src/info/mod.rs`), since `ava-api → ava-version`, not the reverse.

### Deferred: `differential::version_interop` (`specs/26` §9(4))

The fourth M9.22 test needs a **live mixed Go+Rust network**. It is DEFERRED until
the mixed-network harness lands in **M9.14** and will live in
`tests/differential/tests/version_interop.rs` (NOT in `ava-version`, a T0 primitive
that must not depend on `ava-differential`/`ava-network`/`ava-api`). An `#[ignore]`d
stub (`version_interop_deferred`) records the deferral in `tests/compat_matrix.rs`.

`tests/proptests.rs` covers crate invariants per `specs/02` §4: `Application`
compare is a total order (oracle = `(major, minor, patch)` tuple ordering;
antisymmetric + transitive; `name` excluded); `is_active` is monotone in `t` and
boundary-inclusive with `fork_at` consistency for the shipped Mainnet/Fuji
configs; and `Fork` chronological `Ord` matches `fork_time` ordering.
