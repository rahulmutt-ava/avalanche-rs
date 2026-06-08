// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

// A BlockInstant deliberately has no `impl Sub<u64>` (nor any raw-integer time
// math). The only way to move it is via `minus`/`plus` taking a `Duration`.
// This must NOT compile.

fn main() {
    let _ = ava_saevm_params::BlockInstant::from_unix(100) - 5u64;
}
