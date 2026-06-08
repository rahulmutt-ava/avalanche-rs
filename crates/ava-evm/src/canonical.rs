// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `CanonicalStore` (G6): single MDBX rw-tx appending headers, bodies,
//! receipts, and the tip pointer, never touching state/trie tables
//! (spec 10 §3/§17.7). Populated by M6.9.
