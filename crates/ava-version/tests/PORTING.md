# PORTING.md — `ava-version`

Parity against avalanchego `version/` and `upgrade/`. One row per upstream Go
test; status `todo` / `wip` / `ported` / `na`. No `wip` rows at the M0.25 exit
gate.

Owning tasks: M0.22 (Application + Compatibility), M0.23 (UpgradeConfig + Fork +
activation schedule).

| Go source (test) | Rust counterpart | Status |
|---|---|---|
| `version/application_test.go` | `tests/version.rs` | todo |
| `version/compatibility_test.go` | `tests/version.rs` | todo |
| `upgrade/upgrade_test.go` | `tests/golden_upgrade.rs` | todo |
