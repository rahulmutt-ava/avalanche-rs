// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Injectable clock — the ONLY place wall-clock time may be read (hazard #5).
//!
//! TODO(M0.12): `Clock` trait + `RealClock` + `MockClock` per
//! `specs/24-determinism-and-clock.md` §B.1 (`now`/`unix`/`unix_time`/`since`/
//! `monotonic`; `MockClock::{at,set,sync,advance}`; `MAX_UNIX_SECS = (1<<63) -
//! 62_135_596_801`). `monotonic()` returns a `tokio::time::Instant` so tests
//! compose with `start_paused`; this task adds the `tokio` dependency. Add the
//! `// determinism-allow: ava-utils::clock` markers here.
