// Copyright (C) 2019, Ava Labs, Inc. All rights reserved.
// See the file LICENSE for licensing terms.

//! `ava-api-macros` — the `#[rpc_service("name")]` attribute macro backing the
//! gorilla-json2 JSON-RPC shim in `ava-api` (specs 12 §3.2, 14 §1.1).
//!
//! Applied to an inherent `impl` block, the macro keeps the block verbatim and
//! additionally emits an inherent method
//!
//! ```ignore
//! fn register_rpc(self: ::std::sync::Arc<Self>, registry: &mut ServiceRegistry)
//! ```
//!
//! that registers every `pub async fn(&self, Args) -> Result<Reply, RpcError>`
//! method under `"<name>.<MethodName>"` (the gorilla `Service.Method`
//! convention, where `<MethodName>` is Go's exported PascalCase name, e.g.
//! `info.GetNodeID`). Generating the registration from the impl block is the
//! whole point: the registered method set cannot drift from the trait
//! (specs 12 §3.2).
//!
//! Each registered handler:
//! - clones the service `Arc` so the boxed closure is `'static`,
//! - deserializes the single gorilla `params[0]` object into `Args` — a failure
//!   surfaces as a `-32602` (`E_BAD_PARAMS`) `RpcError` (14 §16.1),
//! - awaits `self.method(args)`,
//! - serializes the `Reply` back to `serde_json::Value` — an (unexpected)
//!   serialize failure surfaces as a `-32603` (`E_INTERNAL`) `RpcError`.
//!
//! `RpcError`, `ServiceRegistry`, and the JSON-RPC value types are provided by
//! `ava-api` (`ava_api::jsonrpc`); the macro refers to them through the call
//! site, so the consuming module must have them in scope.

#![forbid(unsafe_code)]

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    Error, FnArg, ImplItem, ItemImpl, LitStr, ReturnType, Type, Visibility, parse_macro_input,
};

/// `#[rpc_service("name")]` — register an impl block's `pub async fn`s as
/// gorilla JSON-RPC methods under the given service `name`.
///
/// See the crate docs for the full contract. The argument is the lowercase
/// service segment of `Service.Method` (e.g. `"info"`, `"health"`).
#[proc_macro_attribute]
pub fn rpc_service(attr: TokenStream, item: TokenStream) -> TokenStream {
    let service_name = parse_macro_input!(attr as LitStr);
    let input = parse_macro_input!(item as ItemImpl);

    match expand(&service_name, &input) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// Builds the original impl block plus the generated `register_rpc` method.
fn expand(service_name: &LitStr, input: &ItemImpl) -> Result<TokenStream2, Error> {
    let self_ty = &input.self_ty;
    let (impl_generics, _ty_generics, where_clause) = input.generics.split_for_impl();

    let mut registrations = Vec::new();
    for item in &input.items {
        if let ImplItem::Fn(method) = item {
            // Only `pub async fn` methods are exposed as RPC endpoints; other
            // helpers on the same impl block are left untouched.
            if !matches!(method.vis, Visibility::Public(_)) || method.sig.asyncness.is_none() {
                continue;
            }
            registrations.push(build_registration(service_name, method)?);
        }
    }

    Ok(quote! {
        #input

        impl #impl_generics #self_ty #where_clause {
            /// Registers each `#[rpc_service]` method on `self` into `registry`
            /// under `"<service>.<Method>"` (generated; see `ava-api-macros`).
            pub fn register_rpc(
                self: ::std::sync::Arc<Self>,
                registry: &mut ServiceRegistry,
            ) {
                #(#registrations)*
            }
        }
    })
}

/// Builds the `registry.register(...)` call for a single async method.
fn build_registration(
    service_name: &LitStr,
    method: &syn::ImplItemFn,
) -> Result<TokenStream2, Error> {
    let method_ident = &method.sig.ident;
    // The Rust method is snake_case (`get_node_id`); the gorilla wire name is
    // Go's exported PascalCase (`GetNodeID`). We convert snake_case ->
    // PascalCase. The exact casing does not affect matching — the method
    // segment is matched case-insensitively (14 §1.1), realized by the registry
    // lowercasing both the registered key and the incoming segment — so
    // `get_node_id` -> `GetNodeId` -> `getnodeid` matches a client's
    // `getNodeID`/`GetNodeID`/`getnodeid` alike.
    let go_method = pascalize(&method_ident.to_string());
    let wire_method = format!("{}.{}", service_name.value(), go_method);

    // The receiver must be `&self`.
    match method.sig.inputs.first() {
        Some(FnArg::Receiver(recv)) if recv.reference.is_some() && recv.mutability.is_none() => {}
        _ => {
            return Err(Error::new_spanned(
                &method.sig,
                "#[rpc_service] methods must take `&self`",
            ));
        }
    }

    // Locate the single `Args` parameter (everything after `&self`). gorilla
    // methods take exactly one argument object; a parameterless Go method maps
    // to a Rust method taking a unit-like args struct, so exactly one typed
    // argument is required here.
    let typed_args: Vec<&syn::PatType> = method
        .sig
        .inputs
        .iter()
        .filter_map(|arg| match arg {
            FnArg::Typed(pat) => Some(pat),
            FnArg::Receiver(_) => None,
        })
        .collect();
    let arg_ty: &Type = match typed_args.as_slice() {
        [single] => &single.ty,
        _ => {
            return Err(Error::new_spanned(
                &method.sig,
                "#[rpc_service] methods must take exactly one argument after `&self`",
            ));
        }
    };

    // The return type must be `Result<Reply, RpcError>`; we only need to know it
    // is a `Result` so the generated body can `?`/match on it. The reply type is
    // serialized via `serde_json::to_value`.
    if matches!(method.sig.output, ReturnType::Default) {
        return Err(Error::new_spanned(
            &method.sig,
            "#[rpc_service] methods must return `Result<Reply, RpcError>`",
        ));
    }

    Ok(quote! {
        {
            let service = ::std::sync::Arc::clone(&self);
            registry.register(#wire_method, move |params: ::serde_json::Value| {
                let service = ::std::sync::Arc::clone(&service);
                ::std::boxed::Box::pin(async move {
                    // gorilla v1 codec: params[0] is the single Args object; a
                    // unmarshal failure is `E_BAD_PARAMS` (-32602), matching
                    // `errInvalidArg` (14 §16.1). An absent / empty params array
                    // arrives here as `null`; gorilla's `*struct{}` methods
                    // accept that, so we deserialize an empty object in its
                    // place (this also lets an all-default Args succeed).
                    let params = if params.is_null() {
                        ::serde_json::Value::Object(::serde_json::Map::new())
                    } else {
                        params
                    };
                    let args: #arg_ty = ::serde_json::from_value(params)
                        .map_err(|e| RpcError::invalid_params(e.to_string()))?;
                    let reply = service.#method_ident(args).await?;
                    ::serde_json::to_value(reply)
                        .map_err(|e| RpcError::internal(e.to_string()))
                })
            });
        }
    })
}

/// Converts a snake_case Rust method ident to a PascalCase Go-style name:
/// each `_` is dropped and the following letter uppercased, and the first
/// letter is uppercased (`get_node_id` -> `GetNodeId`). Since the dispatch
/// match is case-insensitive (the registry lowercases keys), the precise inner
/// casing is immaterial — only the letters (no underscores) need to line up.
fn pascalize(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut upper_next = true;
    for ch in name.chars() {
        if ch == '_' {
            upper_next = true;
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}
