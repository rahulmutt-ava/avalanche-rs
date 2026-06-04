// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Uniform sampler — lazy partial Fisher–Yates (`uniform_replacer.go`).
//!
//! TODO(M0.10): port the lazy partial Fisher–Yates with the `drawn` default-map
//! (`get(k, default=k)`) and the exact draw formula; `sample(count)` resets then
//! draws `count` times.
//! Owning spec: `specs/03-core-primitives.md` §4.1.
