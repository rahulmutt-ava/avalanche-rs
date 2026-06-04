// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-codec-derive` — `#[derive(AvaCodec)]` proc-macro for the hand-written
//! linear codec.
//!
//! TODO(M0.15): implement the derive using `syn`/`quote`. Emit per-kind wire
//! encoding from the reflectcodec rule table (`specs/03-core-primitives.md`
//! §2.4): ints BE, bool 0/1, `String` u16+UTF-8, `[u8;N]` raw, `Vec<u8>`
//! u32+bytes, `Vec<T>` u32 count + elements, structs = concatenated fields,
//! interface enums = `u32` typeID + value, `Box<T>` transparent. Reject
//! `Option<T>` on serialized fields (compile error). Support the `#[codec(...)]`
//! attributes (`type_id`, `type_registry`, `skip_ids`, `version`). Generate an
//! exact `size()`.
//!
//! Scaffolded in M0.1 as a NO-OP derive (expands to nothing) so the workspace
//! and the `ava-codec` re-export compile. Real expansion lands in M0.15.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;

/// `#[derive(AvaCodec)]` — linear-codec (de)serialization.
///
/// Scaffold stub: currently expands to nothing. Implemented in M0.15.
#[proc_macro_derive(AvaCodec, attributes(codec))]
pub fn derive_ava_codec(_input: TokenStream) -> TokenStream {
    // TODO(M0.15): parse `_input` with `syn` and emit `Serializable` /
    // `Deserializable` impls per specs/03 §2.4.
    TokenStream::new()
}
