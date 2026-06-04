// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! Versioned codec `Manager` + the object-safe `Serializable`/`Deserializable`
//! and `Codec` traits.
//!
//! TODO(M0.15): define object-safe `Serializable { marshal_into(&self, &mut
//! Packer); size(&self) -> usize }` and `Deserializable { unmarshal_from(&mut
//! self, &mut Packer) }`.
//! TODO(M0.16): `Manager { max_size, codecs: RwLock<HashMap<u16, Arc<dyn
//! Codec>>> }` with `VERSION_SIZE = 2`, `DEFAULT_MAX_SIZE = 256*1024`,
//! `register` / `marshal` (2-byte version prefix) / `unmarshal` (reject
//! `> max_size`; require `offset == len` -> `ExtraSpace`) / `size`.
//! Owning spec: `specs/03-core-primitives.md` §2.2.
