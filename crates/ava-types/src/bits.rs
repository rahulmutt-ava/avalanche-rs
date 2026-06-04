// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Consensus-affecting bit-subset helpers over ids.
//!
//! TODO(M0.5): port `equal_subset` / `first_difference_subset` verbatim from Go
//! (`ids/bits.go`). These mask id bits for the consensus polling routines and
//! must be bit-exact.
//! Owning spec: `specs/03-core-primitives.md` §1.2.
