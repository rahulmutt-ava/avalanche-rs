# ava-indexer — Go test porting map (M8.24)

Go reference: `avalanchego/indexer/` @ `5896c92fee23c2eff53d557dceeb89f1a6218224`
(also the `goCommit` provenance of `vectors/indexer/indexer_parity.json`).

Status legend: ✅ ported · 🟡 partially ported / covered elsewhere · n/a not applicable.

| Go test (`indexer/*_test.go`) | Rust test | Status | Notes |
|---|---|---|---|
| `index_test.go::TestIndex` | `src/index.rs::tests::accept_ordering_and_markers` | ✅ | per-accept assertions on all five read paths + versiondb-per-run restart, non-decreasing timestamps (deterministic `MockClock` instead of wall clock), raw `nextAcceptedIndex` (key `0x00`) layout check |
| `index_test.go::TestIndexGetContainerByRangeMaxPageSize` | `src/index.rs::tests::get_container_range_bounds` | ✅ | 1025 inserts; cap/zero/start-bound errors asserted as Go-byte-stable strings; overlapping-window equality |
| `index_test.go::TestDontIndexSameContainerTwice` | `src/index.rs::tests::dont_index_same_container_twice` | ✅ | restart-replay dedupe: first write wins |
| `indexer_test.go::TestNewIndexer` | `src/indexer.rs::tests::has_run_marker_and_shutdown` | 🟡 | construction state folded into the marker/shutdown test (Rust has no exported field set to introspect; accessors asserted) |
| `indexer_test.go::TestMarkHasRunAndShutdown` | `src/indexer.rs::tests::has_run_marker_and_shutdown` | ✅ | `hasRun` (key `0x07`) persisted across runs; `shutdown_f` fires on close; idempotent close |
| `indexer_test.go::TestIndexer` | `src/indexer.rs::tests::register_chain_and_accept` + `tests/differential_indexer_parity.rs::indexer_parity` | ✅ | Snowman + DAG registration, route set (`index/<alias>` × `/block,/vtx,/tx`), accepts through the broadcast `AcceptorGroup` (async task + `spawn_blocking`, polled), cross-index isolation, restart state; the differential test additionally pins the FULL physical DB state byte-for-byte vs Go |
| `indexer_test.go::TestIncompleteIndex` | `src/indexer.rs::tests::incomplete_index_fatal` | ✅ | all four runs: mark-incomplete (disabled, never indexed), fatal (re-enabled, incomplete, disallowed), allowed-incomplete proceed, fatal (disabled after indexed); fatal closes + fires `shutdown_f` |
| `indexer_test.go::TestIgnoreNonDefaultChains` | `src/indexer.rs::tests::ignore_non_primary_chains` | ✅ | non-Primary-Network subnet skipped: no index, no routes |
| `client_test.go::*` (index API client) | — | n/a | the Go HTTP client is not ported (wallet/SDK clients are out of M8.24 scope); the service wire shape is covered by `src/service.rs::tests` + the differential reply-JSON checks |
| `examples/*` | — | n/a | documentation samples |

Rust-only additions (no Go counterpart):

| Rust test | Covers |
|---|---|
| `src/container.rs::tests::{container_codec_layout, container_unmarshal_rejects_trailing_bytes}` | hand-asserted linear-codec layout + ExtraSpace check (Go relies on codec package tests) |
| `src/service.rs::tests::index_method_set` | the 6 registered wire names incl. the `GetContainerByID` acronym override (gorilla exact-remainder matching) |
| `src/service.rs::tests::{get_last_accepted_reply_shape, get_container_range_cap, id_lookups, encoding_forms, timestamp_rfc3339_nano_trimming}` | FormattedContainer JSON shape, 1024 cap over the wire, `not found`/`isAccepted=false` semantics, Go `formatting.Encoding` JSON forms, RFC3339Nano trimming |
| `tests/differential_indexer_parity.rs::indexer_parity` | recorded-oracle parity: Container codec bytes, full memdb dumps after runs 1–2, reply JSON via the live JSON-RPC route, all reachable error strings, run-3 fatal (see the test's module docs for the (re)emit procedure) |
