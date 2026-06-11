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
- `avalanche_process_process_network_{receive,transmit}_bytes_total` — emitted
  only by client_golang v1.23.0's **Linux** procfs collector (not on darwin);
  not emitted by the Rust `prometheus` 0.13 collector on any platform (crate
  gap).

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
emitted on `darwin`. Note the collectors are **not** platform-identical:
client_golang v1.23.0's Linux (procfs) process collector emits 2 extra
families — `process_network_receive_bytes_total` and
`process_network_transmit_bytes_total` — that the darwin collector does not;
the Rust `prometheus` 0.13.4 process collector emits neither, on any platform.
Both are therefore explicitly waived in the test, so a snapshot regenerated on
Linux stays green (`go_*` families are waived regardless).

Keep the Rust tree in `golden_metrics_names.rs::rust_schema()` and the Go tree
in the oracle **in sync** — they must build the same namespaces/families.

**Known Go-observable divergences (error paths only):**

- Gather-error message strings differ from client_golang's (Rust error text is
  not a transliteration of `prometheus.Gatherers.Gather`'s).
- Non-GET `/ext/metrics` returns 405 (Go's promhttp serves any method; spec 14
  §6 prescribes GET).
- No gzip content-negotiation (Go's promhttp gzips on `Accept-Encoding`; the
  plain text output is spec-compliant either way).
- Empty metric families are not filtered from the merged output (Go's
  `NormalizeMetricFamilies` drops them).
