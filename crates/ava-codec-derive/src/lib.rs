// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-codec-derive` — `#[derive(AvaCodec)]` proc-macro for the hand-written
//! linear codec.
//!
//! Generates [`Serializable`]/[`Deserializable`] impls that emit the byte-exact
//! wire format described in `specs/03-core-primitives.md` §2.4. The macro never
//! reflects at runtime: each `#[codec]` field expands to a direct
//! `field.marshal_into(p)` / `field.unmarshal_from(p)` call against the
//! pre-existing primitive impls in `ava-codec`.
//!
//! ## Supported shapes
//!
//! - **Structs:** serialize each `#[codec]`-tagged field in declaration order;
//!   untagged fields are skipped. `size()` is the sum of the tagged fields'
//!   sizes.
//! - **Interface enums** (`#[codec(type_registry)]`): each variant carries a
//!   single newtype payload and an explicit `#[codec(type_id = N)]`. Marshal
//!   writes the `u32` typeID then the payload; unmarshal reads the typeID and
//!   dispatches. An inherent `codec_type_id()` method is generated for golden
//!   typeID-table assertions.
//!
//! ## Rejected at compile time
//!
//! - `Option<T>` on a serialized field — there is no presence byte on the
//!   Avalanche wire (`specs/03` §2.4 "Pointers / Option").

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitInt, Type, parse_macro_input};

/// `#[derive(AvaCodec)]` — linear-codec (de)serialization.
///
/// See the crate docs for the supported shapes and the wire format.
#[proc_macro_derive(AvaCodec, attributes(codec))]
pub fn derive_ava_codec(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let is_registry = has_type_registry(&input);
    let result = match &input.data {
        Data::Struct(_) if is_registry => Err(syn::Error::new_spanned(
            &input.ident,
            "#[codec(type_registry)] is only valid on enums",
        )),
        Data::Struct(data) => derive_struct(&input, &data.fields),
        Data::Enum(data) if is_registry => derive_registry_enum(&input, data),
        Data::Enum(_) => Err(syn::Error::new_spanned(
            &input.ident,
            "enums require #[codec(type_registry)] to derive AvaCodec",
        )),
        Data::Union(_) => Err(syn::Error::new_spanned(
            &input.ident,
            "AvaCodec cannot be derived for unions",
        )),
    };
    match result {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Whether the top-level type carries `#[codec(type_registry)]`.
fn has_type_registry(input: &DeriveInput) -> bool {
    input
        .attrs
        .iter()
        .any(|attr| attr.path().is_ident("codec") && registry_flag_present(attr))
}

/// Confirms the `type_registry` token is present in a `#[codec(...)]` attribute.
fn registry_flag_present(attr: &syn::Attribute) -> bool {
    let mut found = false;
    let _ = attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("type_registry") {
            found = true;
        }
        // Consume any `key = value` form to avoid parse errors.
        if meta.input.peek(syn::Token![=]) {
            let _: syn::Expr = meta.value()?.parse()?;
        }
        Ok(())
    });
    found
}

// ----- struct derivation -----

/// Returns `true` if a field is tagged `#[codec]` (with or without nested args).
fn field_is_tagged(field: &syn::Field) -> bool {
    field.attrs.iter().any(|a| a.path().is_ident("codec"))
}

/// Rejects `Option<T>` fields (no presence byte on the wire).
fn reject_option(ty: &Type) -> Result<(), syn::Error> {
    if let Type::Path(tp) = ty
        && tp
            .path
            .segments
            .last()
            .is_some_and(|seg| seg.ident == "Option")
    {
        return Err(syn::Error::new_spanned(
            ty,
            "Option<T> is not allowed on a #[codec] field: the Avalanche \
             wire has no presence byte (specs/03 §2.4)",
        ));
    }
    Ok(())
}

fn derive_struct(input: &DeriveInput, fields: &Fields) -> Result<TokenStream2, syn::Error> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let named = match fields {
        Fields::Named(f) => &f.named,
        Fields::Unit => {
            // A unit struct serializes to zero bytes.
            return Ok(empty_impl(input));
        }
        Fields::Unnamed(_) => {
            return Err(syn::Error::new_spanned(
                name,
                "AvaCodec structs must have named fields",
            ));
        }
    };

    let mut marshal = Vec::new();
    let mut unmarshal = Vec::new();
    let mut size_terms = Vec::new();

    for field in named {
        if !field_is_tagged(field) {
            continue;
        }
        reject_option(&field.ty)?;
        let ident = field
            .ident
            .as_ref()
            .ok_or_else(|| syn::Error::new_spanned(field, "field must be named"))?;
        marshal.push(quote! {
            ::ava_codec::Serializable::marshal_into(&self.#ident, p);
        });
        unmarshal.push(quote! {
            ::ava_codec::Deserializable::unmarshal_from(&mut self.#ident, p);
        });
        size_terms.push(quote! {
            total = total.saturating_add(::ava_codec::Serializable::size(&self.#ident));
        });
    }

    Ok(quote! {
        impl #impl_generics ::ava_codec::Serializable for #name #ty_generics #where_clause {
            fn marshal_into(&self, p: &mut ::ava_codec::packer::Packer) {
                #(#marshal)*
            }
            fn size(&self) -> usize {
                let mut total: usize = 0;
                #(#size_terms)*
                total
            }
        }
        impl #impl_generics ::ava_codec::Deserializable for #name #ty_generics #where_clause {
            fn unmarshal_from(&mut self, p: &mut ::ava_codec::packer::Packer) {
                #(#unmarshal)*
            }
        }
    })
}

/// Impls for a zero-field (unit) struct.
fn empty_impl(input: &DeriveInput) -> TokenStream2 {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    quote! {
        impl #impl_generics ::ava_codec::Serializable for #name #ty_generics #where_clause {
            fn marshal_into(&self, _p: &mut ::ava_codec::packer::Packer) {}
            fn size(&self) -> usize { 0 }
        }
        impl #impl_generics ::ava_codec::Deserializable for #name #ty_generics #where_clause {
            fn unmarshal_from(&mut self, _p: &mut ::ava_codec::packer::Packer) {}
        }
    }
}

// ----- enum (interface registry) derivation -----

/// Extracts the explicit `#[codec(type_id = N)]` for an enum variant.
fn variant_type_id(variant: &syn::Variant) -> Result<u32, syn::Error> {
    for attr in &variant.attrs {
        if !attr.path().is_ident("codec") {
            continue;
        }
        let mut id: Option<u32> = None;
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("type_id") {
                let lit: LitInt = meta.value()?.parse()?;
                id = Some(lit.base10_parse()?);
            } else if meta.path.is_ident("skip_ids") {
                // skip_ids only affects the *next* implicit assignment; with
                // explicit type_id on every variant it is documentational. Parse
                // and ignore the value.
                let _: LitInt = meta.value()?.parse()?;
            }
            Ok(())
        })?;
        if let Some(id) = id {
            return Ok(id);
        }
    }
    Err(syn::Error::new_spanned(
        variant,
        "each #[codec(type_registry)] variant requires #[codec(type_id = N)]",
    ))
}

fn derive_registry_enum(
    input: &DeriveInput,
    data: &syn::DataEnum,
) -> Result<TokenStream2, syn::Error> {
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let mut marshal_arms = Vec::new();
    let mut size_arms = Vec::new();
    let mut type_id_arms = Vec::new();
    // (type_id, variant_ident, payload_ty) for the unmarshal dispatch.
    let mut dispatch = Vec::new();

    for variant in &data.variants {
        let vid = &variant.ident;
        let type_id = variant_type_id(variant)?;
        let payload_ty = match &variant.fields {
            Fields::Unnamed(f) if f.unnamed.len() == 1 => f
                .unnamed
                .first()
                .map(|field| field.ty.clone())
                .ok_or_else(|| syn::Error::new_spanned(variant, "missing payload type"))?,
            _ => {
                return Err(syn::Error::new_spanned(
                    variant,
                    "type_registry variants must be a single-field tuple variant, e.g. Variant(T)",
                ));
            }
        };

        marshal_arms.push(quote! {
            #name::#vid(inner) => {
                p.pack_u32(#type_id);
                ::ava_codec::Serializable::marshal_into(inner, p);
            }
        });
        size_arms.push(quote! {
            #name::#vid(inner) => {
                ::ava_codec::packer::INT_LEN
                    .saturating_add(::ava_codec::Serializable::size(inner))
            }
        });
        type_id_arms.push(quote! {
            #name::#vid(_) => #type_id,
        });
        dispatch.push((type_id, vid.clone(), payload_ty));
    }

    let dispatch_arms = dispatch.iter().map(|(type_id, vid, payload_ty)| {
        quote! {
            #type_id => {
                let mut inner = <#payload_ty as ::core::default::Default>::default();
                ::ava_codec::Deserializable::unmarshal_from(&mut inner, p);
                if !p.errored() {
                    *self = #name::#vid(inner);
                }
            }
        }
    });

    Ok(quote! {
        impl #impl_generics #name #ty_generics #where_clause {
            /// The `u32` codec typeID for this variant (registration order;
            /// asserted against the Go-dumped table).
            #[must_use]
            pub fn codec_type_id(&self) -> u32 {
                match self {
                    #(#type_id_arms)*
                }
            }
        }

        impl #impl_generics ::ava_codec::Serializable for #name #ty_generics #where_clause {
            fn marshal_into(&self, p: &mut ::ava_codec::packer::Packer) {
                match self {
                    #(#marshal_arms)*
                }
            }
            fn size(&self) -> usize {
                match self {
                    #(#size_arms)*
                }
            }
        }

        impl #impl_generics ::ava_codec::Deserializable for #name #ty_generics #where_clause {
            fn unmarshal_from(&mut self, p: &mut ::ava_codec::packer::Packer) {
                let type_id = p.unpack_u32();
                if p.errored() {
                    return;
                }
                match type_id {
                    #(#dispatch_arms)*
                    _ => {
                        p.add_external_error(::ava_codec::error::PackerError::InvalidInput);
                    }
                }
            }
        }
    })
}
