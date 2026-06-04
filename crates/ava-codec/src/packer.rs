// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Big-endian primitive reader/writer with sticky-error semantics.
//!
//! TODO(M0.14): implement `Packer` per `specs/03-core-primitives.md` §2.1:
//! constants (`BYTE_LEN..LONG_LEN`, `MAX_STRING_LEN = u16::MAX`), an owned-Vec
//! (write) / borrowed-slice (read) buffer, `offset`, `max_size`, the
//! first-error-wins sticky `Option<PackerError>`, and the
//! `pack_*` / `unpack_*` / `unpack_limited_*` methods. `pack_bool`/`unpack_bool`
//! accept 0/1 only; once errored, ops no-op and return zero.
