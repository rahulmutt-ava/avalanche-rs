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
| `unary_snowball.go` / `unary_snowball_test.go` | `src/snowball/unary_snowball.rs` | wip | primitive code ported; `TestUnarySnowball` golden vector not yet asserted |
| `nnary_snowball.go` / `nnary_snowball_test.go` | `src/snowball/nnary_snowball.rs` | wip | primitive code ported; `TestNnarySnowball` golden vector not yet asserted |
| `*_test.go` error-driven suites (`getErrorDrivenSnowflake*Suite`) | — | todo | confidence-vector helper suites (terminate/reset/switch); follow-up after M3.3 |
| `flat.go` / `flat_test.go` | — | todo | flat consensus instance (not on M3.1–M3.3 path) |
| `tree.go` / `tree_test.go` | — | todo | M3.4 |
| `consensus_test.go` (Red/Blue/Green + Byzantine) | — | todo | shared color ids + Byzantine suite at M3.4/M3.5 |

## Snowman / topological (`snow/consensus/snowman/`)

| Go file | Rust location | Status | Notes |
|---|---|---|---|
| `consensus_test.go` | — | wip | safety battery target at M3.5 |
| `network_test.go` | — | wip | metastable oracle for `prop::consensus_safety`; harness scaffolded at M3.1 (`tests/prop_safety.rs`, `#[ignore]`d) |
| `topological.go` | — | todo | M3.5 |
