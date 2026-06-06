# ava-snow — Go test-corpus porting status

Tracks which avalanchego `snow/` test files have been ported into `ava-snow`.
Status: `done` (ported + green), `wip` (in progress / partially ported),
`todo` (not started). Go source root: `../avalanchego/snow/...`.

## snowball primitives (`snow/consensus/snowball/`)

Legend: `done` = code ported **and** a Go-derived golden vector asserts it green;
`wip` = code ported but its golden vector(s) not yet asserted; `todo` = not started.

| Go file | Rust location | Status | Notes |
|---|---|---|---|
| `parameters.go` / `parameters_test.go` | `src/snowball/parameters.rs`, `tests/golden_snowball.rs::golden_parameters_verify` | done | full 16-case `TestParametersVerify` table ported; exact branch order incl. the `alpha_confidence==3 && alpha_preference==28` easter egg; `DEFAULT_PARAMETERS` validity asserted |
| `binary_slush.go` | `src/snowball/binary_slush.rs` | done | embedded in binary snowflake; exercised transitively by `golden_binary_snowflake` |
| `binary_snowflake.go` / `binary_snowflake_test.go` | `src/snowball/binary_snowflake.rs`, `tests/golden_snowball.rs::golden_binary_snowflake` | done | top-level `TestBinarySnowflake` ported (preference/finalized). Error-driven single/multi-choice suites NOT yet ported |
| `binary_snowball.go` / `binary_snowball_test.go` | `src/snowball/binary_snowball.rs`, `tests/golden_snowball.rs::golden_binary_snowball{,_record_poll_preference}` | done | `TestBinarySnowball` + `TestBinarySnowballRecordPollPreference` (incl. the `[4,1]` strength split) ported. `RecordUnsuccessfulPoll`/`AcceptWeird`/`Lock`/`String` variants NOT yet ported |
| `unary_snowflake.go` / `unary_snowflake_test.go` | `src/snowball/unary_snowflake.rs`, `tests/golden_snowball.rs::golden_unary_snowflake` | done | top-level `TestUnarySnowflake` ported (confidence vector + extend-to-binary). Error-driven suite NOT yet ported |
| `nnary_slush.go` | `src/snowball/nnary_snowflake.rs` | done | embedded in n-nary snowflake; exercised by `golden_nnary_snowflake` |
| `nnary_snowflake.go` / `nnary_snowflake_test.go` | `src/snowball/nnary_snowflake.rs`, `tests/golden_snowball.rs::golden_nnary_snowflake` | done | top-level `TestNnarySnowflake` ported. Error-driven suite NOT yet ported |
| `unary_snowball.go` / `unary_snowball_test.go` | `src/snowball/unary_snowball.rs`, `tests/golden_tree.rs` | done | primitive + `Display`/`UnaryInstance` ported; exercised end-to-end (preference-strength + extend-to-binary + `String()`) by the M3.4 tree golden vectors (`record_preference_poll_unary`, `fine_grained`, …) |
| `nnary_snowball.go` / `nnary_snowball_test.go` | `src/snowball/nnary_snowball.rs` | wip | primitive + `Display`/`NnaryInstance` ported; the snowball `Tree` uses unary/binary instances, so the n-nary `String()`/`record_poll` golden vector (`TestNnarySnowball`) is still not asserted. Carry to a later snowball pass |
| `*_test.go` error-driven suites (`getErrorDrivenSnowflake*Suite`) | — | todo | confidence-vector helper suites (terminate/reset/switch); follow-up after M3.3 |
| `flat.go` / `flat_test.go` | — | todo | flat consensus instance (not on M3.1–M3.3 path) |
| `consensus.go` / `factory.go` | `src/snowball/consensus.rs` | done | `Consensus`/`Factory`/`NnaryInstance`/`BinaryInstance`/`UnaryInstance` traits + `SnowballFactory`/`SnowflakeFactory` (M3.4) |
| `tree.go` / `tree_test.go` | `src/snowball/tree.rs`, `tests/golden_tree.rs` | done | M3.4: full Patricia `Tree` (`Consensus`) + 5 `Add` split cases + `should_reset` falter + byte-exact `Display`. 12 tree golden vectors ported (singleton/binary/first/last/fine-grained/trinary/transitive-reset/etc.) |
| `consensus_test.go` (Red/Blue/Green + Byzantine) | `tests/conformance_battery.rs` (Snowman) | wip | shared color ids reused in `tests/golden_tree.rs`; the Snowman consensus battery lands at M3.5 |

## Snowman / topological (`snow/consensus/snowman/`)

| Go file | Rust location | Status | Notes |
|---|---|---|---|
| `consensus.go` | `src/snowman/consensus.rs` | done | `SnowmanConsensus` trait (M3.5); metrics-only `Initialize`/health folded into `Topological::new` + `health_check` |
| `block.go` / `snowman_block.go` | `src/snowman/block.rs`, `src/snowman/topological.rs` (`SnowmanBlock`) | done | synchronous consensus-internal `Block`/`BlockAcceptor`; `SnowmanBlock` (`blk`/`should_falter`/`sb`/`children`) |
| `topological.go` | `src/snowman/topological.rs` | done | M3.5: `Topological` (exact field set) + `add`/`record_poll` (Kahn `calculate_in_degree`/`push_votes`/`vote`) + `accept_preferred_child` (acceptor-before-`accept` invariant) + `reject_transitively` + `health_check` |
| `consensus_test.go` | `src/snowtest.rs` (`run_consensus_suite`), `tests/conformance_battery.rs` | done | M3.5: 19-case generic battery vs `Topological` (init/add/record_poll/accept-ordering/dup-add/unknown-parent/linear acceptance/sibling reject/transitive reject/preference walk/last-accepted). NOTE: `RecordPollTransitivelyResetConfidence`'s intermediate preference assertion is id-bit-layout-dependent in Go (random `BuildChild` ids); the Rust port asserts the structurally-invariant facts (num_processing + final acceptance) — see finding |
| `network_test.go` | `tests/prop_safety.rs`, `src/testutil/cluster.rs` | done | M3.5: `Topological` wired into the cluster (one instance per node + oracle acceptor); `prop::consensus_safety` UN-IGNORED and GREEN (64 cases) |
