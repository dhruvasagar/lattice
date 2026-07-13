//! `#[derive(PluginEvent)]` — the guest-side derive for plugin-defined events
//! (PH7.8b.3). The companion proc-macro crate to `lattice-plugin-sdk` (the
//! serde/serde_derive split: a proc-macro crate cannot also export a normal
//! library, so the runtime trait lives in `lattice-plugin-sdk`, which re-exports
//! this derive).
//!
//! The derive is **WIT-agnostic** (PH7.8b.3 approach A): it generates a
//! `PluginEvent` impl over the opaque MessagePack wire and touches no plugin-host
//! bindings. A plugin emits with its own generated `host-services emit-event`
//! using the derived `NAME` + `encode()`; the derive only supplies the type-safe
//! encode/decode + the name/doc constants.
//!
//! Generated for `#[derive(PluginEvent)] struct Foo { .. }`:
//!   - `NAME` — from `#[event(name = "...")]`, else the type name kebab-cased.
//!   - `DOC` — the struct's `///` doc-comment (the doc-comment IS the event doc,
//!     the PI.4 "doc from doc comments" principle applied to events).
//!   - `encode` / `decode` — MessagePack via `lattice-plugin-sdk`'s private
//!     helpers, so the consumer deps only the SDK (not `rmp-serde` directly).
//!
//! The consumer must also `#[derive(serde::Serialize, serde::Deserialize)]` —
//! the encode/decode bodies require it. Generated code references
//! `::lattice_plugin_sdk`, so the consumer must depend on `lattice-plugin-sdk`
//! (which uses `extern crate self as lattice_plugin_sdk;` so the paths resolve in
//! its own tests).

use proc_macro::TokenStream;
use quote::quote;
use syn::{Attribute, DeriveInput, LitStr, parse_macro_input};

/// Derive `PluginEvent` for a serde-serializable struct. See the crate docs for
/// the generated items and the `#[event(name = "...")]` / doc-comment inputs.
#[proc_macro_derive(PluginEvent, attributes(event))]
pub fn derive_plugin_event(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ident = &input.ident;

    // NAME: explicit `#[event(name = "...")]` wins; otherwise the type name
    // kebab-cased (real plugins namespace via the attr, e.g. "git.hunks-changed").
    let name = match parse_event_name(&input.attrs) {
        Ok(Some(explicit)) => explicit,
        Ok(None) => to_kebab_case(&ident.to_string()),
        Err(err) => return err.to_compile_error().into(),
    };

    // DOC: the struct's `///` doc-comment (joined + trimmed). Empty if none.
    let doc = doc_string(&input.attrs);

    let expanded = quote! {
        impl ::lattice_plugin_sdk::PluginEvent for #ident {
            const NAME: &'static str = #name;
            const DOC: &'static str = #doc;

            fn encode(&self) -> ::std::vec::Vec<u8> {
                ::lattice_plugin_sdk::__private::encode(self)
            }

            fn decode(
                bytes: &[u8],
            ) -> ::core::result::Result<Self, ::lattice_plugin_sdk::DecodeError> {
                ::lattice_plugin_sdk::__private::decode(bytes)
            }
        }
    };
    expanded.into()
}

/// Parse an optional `#[event(name = "...")]` attribute. Returns the explicit
/// name if present, `None` if the attribute is absent, or a `syn::Error` if the
/// attribute is malformed (unknown key / non-string value) — surfaced as a
/// compile error at the derive site, never a silent fallback.
fn parse_event_name(attrs: &[Attribute]) -> Result<Option<String>, syn::Error> {
    let mut found = None;
    for attr in attrs {
        if !attr.path().is_ident("event") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value: LitStr = meta.value()?.parse()?;
                found = Some(value.value());
                Ok(())
            } else {
                Err(meta.error("unknown `event` attribute key (expected `name`)"))
            }
        })?;
    }
    Ok(found)
}

/// Join a struct's `#[doc = "..."]` attribute lines into a single trimmed
/// doc string. Each `///` line contributes one entry; interior blank lines are
/// preserved, leading/trailing whitespace trimmed.
fn doc_string(attrs: &[Attribute]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        let syn::Meta::NameValue(nv) = &attr.meta else {
            continue;
        };
        let syn::Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) = &nv.value
        else {
            continue;
        };
        // Rust stores the leading space after `///` in the literal; trim one
        // layer of surrounding whitespace per line.
        lines.push(s.value().trim().to_string());
    }
    lines.join("\n").trim().to_string()
}

/// Convert a PascalCase type name to kebab-case (`MyEventType` → `my-event-type`).
/// A boundary is any uppercase char (except the first); acronyms degrade to
/// per-letter kebab, which is why real plugins set an explicit `#[event(name)]`.
fn to_kebab_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, ch) in s.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('-');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}
