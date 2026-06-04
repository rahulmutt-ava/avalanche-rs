// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Weighted sampler — cumulative-weight heap (`weighted_heap.go`).
//!
//! TODO(M0.10): heap of `{weight, cumulative_weight, index}`, stable-sort
//! `(weight desc, index asc)`, accumulate with CHECKED add (`parent=(i-1)>>1`),
//! traversal exactly as Go.
//! Owning spec: `specs/03-core-primitives.md` §4.1.
