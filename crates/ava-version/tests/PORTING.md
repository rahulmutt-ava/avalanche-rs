# PORTING.md — `ava-version`

Parity against avalanchego `version/` and `upgrade/`. One row per upstream Go
test; status `todo` / `wip` / `ported` / `na`. No `wip` rows at the M0.25 exit
gate.

Owning tasks: M0.22 (Application + Compatibility), M0.23 (UpgradeConfig + Fork +
activation schedule), M0.24 (proptest suite).

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `version/application_test.go` | `tests/version.rs` | ported |
| `version/compatibility_test.go` | `tests/version.rs` | ported |
| `upgrade/upgrade_test.go` | `tests/golden_upgrade.rs` | ported |
| _(proptest invariants — no direct Go counterpart; `specs/02` §4)_ | `tests/proptests.rs` | ported |

`tests/proptests.rs` covers crate invariants per `specs/02` §4: `Application`
compare is a total order (oracle = `(major, minor, patch)` tuple ordering;
antisymmetric + transitive; `name` excluded); `is_active` is monotone in `t` and
boundary-inclusive with `fork_at` consistency for the shipped Mainnet/Fuji
configs; and `Fork` chronological `Ord` matches `fork_time` ordering.
