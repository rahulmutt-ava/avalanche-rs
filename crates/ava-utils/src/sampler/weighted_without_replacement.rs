// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Weighted-without-replacement sampler (generic over uniform + weighted).
//!
//! TODO(M0.10): `initialize` sums weights with checked add; `sample(count)` =
//! reset uniform, then `weighted.sample(uniform.next())` per draw. Provide
//! `new_deterministic_weighted_without_replacement(src)`. Never repeats an index.
//! Owning spec: `specs/03-core-primitives.md` §4.1.
