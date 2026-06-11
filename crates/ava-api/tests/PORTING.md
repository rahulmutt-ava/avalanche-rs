# ava-api test vectors — porting notes

## `vectors/api/metrics_schema.json` — metrics-name golden (M8.21, specs 18 §3/§4)

**What it is.** The Go `/ext/metrics` schema snapshot
`{(name, type, sorted(label_keys))}` — values dropped, schema only — emitted by
the **real** Go `api/metrics` gatherer tree via the in-repo oracle
`go-oracle/metrics_schema_oracle_test.go`. `golden_metrics_names.rs::
metrics_name_parity` rebuilds the identical tree with `ava_api::metrics` and
asserts the Rust schema is a **superset** of every non-waived Go family.

**Scope decision.** Spec 18 §3 prescribes snapshotting a fully booted node's
`/ext/metrics`. That full-node differential run is the `02`-harness's job
(post-M8.29, when `avalanchers` can boot all chains); it is not feasible — and
not honest — from an `ava-api` unit test, because the per-subsystem families
(18 §2.1–§2.15) are registered by their owning crates (M2–M7), not by
`ava-api`. This per-PR golden is therefore scoped to what the gatherer/naming
machinery itself produces, built from real Go code (never hand-fabricated
names):

- `avalanche_process` — node.go `initMetricsAPI`'s collectors
  (`collectors.NewProcessCollector` + `collectors.NewGoCollector`) under
  `MakeAndRegister`. This captures the real (and spec-correcting) names:
  the prefix gatherer renames unconditionally, so the families are
  `avalanche_process_process_*` / `avalanche_process_go_*` — **not** bare
  `process_*`/`go_*` as 18 §4's parenthetical suggests.
- `avalanche_network` — a representative subsystem registry
  (`peers` gauge, `peers_subnet{subnetID}` gauge vec; 18 §2.1).
- `avalanche_snowman` — the chains/manager.go per-chain wiring: a
  `LabelGatherer("chain")` registered into the root prefix gatherer, with a
  chain registry under alias `P` (`polls_successful`/`polls_failed`; 18 §2.8).

**Waivers (documented in the test, 18 §4):**

- `avalanche_process_go_*` — Go-runtime collector; no Rust equivalent, never
  faked.
- `avalanche_process_process_*` off Linux — the Rust `prometheus` crate's
  process collector is Linux-only; full `process_*` parity is asserted on
  Linux (the production target).
- `avalanche_process_process_virtual_memory_max_bytes` — not emitted by the
  Rust `prometheus` 0.13 process collector (crate gap), on any platform.

**Regenerate** (avalanchego checkout required; leaves the Go tree clean):

```sh
AG=/path/to/avalanchego
RS=/path/to/avalanche-rs
cp $RS/crates/ava-api/tests/go-oracle/metrics_schema_oracle_test.go $AG/api/metrics/
cd $AG
AVAX_RS_GO_COMMIT=$(git rev-parse HEAD) \
AVAX_RS_METRICS_SCHEMA_OUT=$RS/crates/ava-api/tests/vectors/api/metrics_schema.json \
  go test ./api/metrics/ -run TestEmitAvalancheRsMetricsSchema -count=1 -v
rm $AG/api/metrics/metrics_schema_oracle_test.go
```

Current snapshot provenance: avalanchego `5896c92fee23c2eff53d557dceeb89f1a6218224`,
emitted on `darwin` (the Go process collector emits the same 7 `process_*`
families on darwin as on linux since client_golang v1.20; `go_*` families are
waived regardless).

Keep the Rust tree in `golden_metrics_names.rs::rust_schema()` and the Go tree
in the oracle **in sync** — they must build the same namespaces/families.
