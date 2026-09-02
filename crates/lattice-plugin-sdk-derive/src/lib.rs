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
use syn::{Attribute, Data, DeriveInput, Fields, LitStr, Type, parse_macro_input};

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
        } else if ch == '_' {
            // TC.4: `ConfigShape` kebab-cases FIELD names, which are
            // `snake_case` where the type and variant names its two older
            // callers pass are `CamelCase`. An underscore is not kebab-case by
            // any reading, so handling it here rather than in a second helper
            // keeps one answer to "what is this called on the wire" — and a
            // type name containing one would have wanted this too.
            out.push('-');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Derive `PluginOption` for a newtype struct over `bool` / `i64` / `String`
/// (PH7.10b) — the guest-side ergonomic layer over the `config.register-option`
/// wire. Like `PluginEvent`, it is WIT-agnostic: it generates only metadata
/// constants + the value type; the plugin makes the `register-option` /
/// `get-option` WIT calls itself using them.
///
/// `#[derive(PluginOption)] #[option(default = "8")] struct TabWidth(i64);`
/// generates:
///   - `NAME` — from `#[option(name = "...")]`, else the type name kebab-cased.
///   - `DOC` — the struct's `///` doc-comment.
///   - `DEFAULT` — the required `#[option(default = "...")]` string.
///   - `KIND` — the `OptionKind` inferred from the field type (`bool`→`Boolean`,
///     `i64`→`Integer`, `String`→`String`).
///   - `type Value` — the field type (parse a `get-option` result via
///     `lattice_plugin_sdk::parse_option`).
#[proc_macro_derive(PluginOption, attributes(option))]
pub fn derive_plugin_option(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ident = &input.ident;

    let value_ty = match newtype_field(&input.data) {
        Ok(ty) => ty,
        Err(err) => return err.to_compile_error().into(),
    };
    let kind = match option_kind_for(&value_ty) {
        Ok(k) => k,
        Err(err) => return err.to_compile_error().into(),
    };

    let (explicit_name, default) = match parse_option_attr(&input.attrs) {
        Ok(pair) => pair,
        Err(err) => return err.to_compile_error().into(),
    };
    let name = explicit_name.unwrap_or_else(|| to_kebab_case(&ident.to_string()));
    let default = match default {
        Some(d) => d,
        None => {
            return syn::Error::new_spanned(
                ident,
                "`#[derive(PluginOption)]` requires `#[option(default = \"...\")]`",
            )
            .to_compile_error()
            .into();
        }
    };
    let doc = doc_string(&input.attrs);

    let expanded = quote! {
        impl ::lattice_plugin_sdk::PluginOption for #ident {
            const NAME: &'static str = #name;
            const DOC: &'static str = #doc;
            const DEFAULT: &'static str = #default;
            const KIND: ::lattice_plugin_sdk::OptionKind = #kind;
            type Value = #value_ty;
        }
    };
    expanded.into()
}

/// Extract the single field type of a newtype (one-field tuple) struct, or a
/// `syn::Error` if the input isn't `struct Foo(T);`.
fn newtype_field(data: &Data) -> Result<Type, syn::Error> {
    if let Data::Struct(s) = data
        && let Fields::Unnamed(f) = &s.fields
        && f.unnamed.len() == 1
    {
        return Ok(f.unnamed[0].ty.clone());
    }
    Err(syn::Error::new(
        proc_macro2::Span::call_site(),
        "`#[derive(PluginOption)]` requires a newtype struct with one field, e.g. `struct TabWidth(i64);`",
    ))
}

/// Map the field type to an `OptionKind` token, or a `syn::Error` for an
/// unsupported type. Matches the last path segment ident (`bool` / `i64` /
/// `String`) — the three native `OptionType` impls.
fn option_kind_for(ty: &Type) -> Result<proc_macro2::TokenStream, syn::Error> {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
    {
        let variant = match seg.ident.to_string().as_str() {
            "bool" => Some(quote! { ::lattice_plugin_sdk::OptionKind::Boolean }),
            "i64" => Some(quote! { ::lattice_plugin_sdk::OptionKind::Integer }),
            "String" => Some(quote! { ::lattice_plugin_sdk::OptionKind::String }),
            _ => None,
        };
        if let Some(v) = variant {
            return Ok(v);
        }
    }
    Err(syn::Error::new_spanned(
        ty,
        "`#[derive(PluginOption)]` field must be `bool`, `i64`, or `String`",
    ))
}

/// Parse `#[option(name = "...", default = "...")]` into `(name?, default?)`. An
/// unknown key or non-string value is a compile error, never a silent fallback.
fn parse_option_attr(attrs: &[Attribute]) -> Result<(Option<String>, Option<String>), syn::Error> {
    let mut name = None;
    let mut default = None;
    for attr in attrs {
        if !attr.path().is_ident("option") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value: LitStr = meta.value()?.parse()?;
                name = Some(value.value());
                Ok(())
            } else if meta.path.is_ident("default") {
                let value: LitStr = meta.value()?.parse()?;
                default = Some(value.value());
                Ok(())
            } else {
                Err(meta.error("unknown `option` attribute key (expected `name` or `default`)"))
            }
        })?;
    }
    Ok((name, default))
}

// ── TC.4: `#[derive(ConfigShape)]` ──────────────────────────────────────────

/// Derive [`ConfigShape`](lattice_plugin_sdk::shape::ConfigShape) for a struct
/// with named fields, or for an enum whose variants are all unit.
///
/// A struct becomes a `record`; each field's schema is its type's, its doc is
/// its `///` comment, and its `required` comes from the TYPE — an `Option<T>`
/// field is optional and everything else is required. That is the reason there
/// is no `#[shape(optional)]` attribute: the type already says it, and a second
/// place to say it is a second place to disagree.
///
/// An all-unit enum becomes an `enum-of` over its variants kebab-cased, which is
/// the spelling `:set` completion and `:customize` show. A data-carrying variant
/// is rejected rather than flattened — a tagged union has no `config-schema`
/// arm, and inventing one silently would produce a shape the host validates
/// against wrongly.
///
/// Field names cross **kebab-cased**, matching the option-name convention
/// (`todo-keywords`, not `todo_keywords`) so a TOML key reads the way every
/// other key in the file does.
#[proc_macro_derive(ConfigShape, attributes(shape))]
pub fn derive_config_shape(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let ident = &input.ident;

    match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => config_shape_for_struct(ident, named),
            _ => syn::Error::new_spanned(
                ident,
                "`#[derive(ConfigShape)]` on a struct requires named fields — a record's fields have names",
            )
            .to_compile_error()
            .into(),
        },
        Data::Enum(e) => config_shape_for_enum(ident, e),
        Data::Union(_) => syn::Error::new_spanned(
            ident,
            "`#[derive(ConfigShape)]` does not support unions",
        )
        .to_compile_error()
        .into(),
    }
}

fn config_shape_for_struct(ident: &syn::Ident, named: &syn::FieldsNamed) -> TokenStream {
    let mut field_schema = Vec::new();
    let mut field_to = Vec::new();
    let mut field_from = Vec::new();
    let mut binds = Vec::new();

    for f in &named.named {
        let Some(fident) = f.ident.as_ref() else {
            continue;
        };
        // `Ident::to_string()` on a RAW identifier yields `r#match`, and a
        // field named after a keyword is not exotic here — org's agenda
        // sections have a `match` field, because that is org's own name for
        // it. Emitting `r#match` as the wire name would have produced a schema
        // whose field no config file could ever spell.
        let wire = to_kebab_case(fident.to_string().trim_start_matches("r#"));
        let doc = doc_string(&f.attrs);
        let ty = &f.ty;
        let optional = is_option_type(ty);

        field_schema.push(quote! {
            ::lattice_plugin_sdk::shape::Field {
                name: #wire.to_string(),
                schema: <#ty as ::lattice_plugin_sdk::shape::ConfigShape>::schema(),
                required: !#optional,
                doc: #doc.to_string(),
            }
        });

        if optional {
            // An absent optional field is OMITTED, not emitted as a placeholder
            // — "absent" and "present but empty" are different values, and the
            // host's `required` check reads absence.
            field_to.push(quote! {
                if let ::core::option::Option::Some(v) = &self.#fident {
                    fields.push((
                        #wire.to_string(),
                        ::lattice_plugin_sdk::shape::ConfigShape::to_value(v),
                    ));
                }
            });
            field_from.push(quote! {
                let #fident = match value.field(#wire) {
                    ::core::option::Option::Some(v) => ::core::option::Option::Some(
                        ::lattice_plugin_sdk::shape::ConfigShape::from_value(v)
                            .map_err(|e| e.under(#wire))?,
                    ),
                    ::core::option::Option::None => ::core::option::Option::None,
                };
            });
        } else {
            field_to.push(quote! {
                fields.push((
                    #wire.to_string(),
                    ::lattice_plugin_sdk::shape::ConfigShape::to_value(&self.#fident),
                ));
            });
            field_from.push(quote! {
                let #fident = {
                    let v = value.field(#wire).ok_or_else(|| {
                        ::lattice_plugin_sdk::shape::ShapeError::new("required field is missing")
                            .under(#wire)
                    })?;
                    ::lattice_plugin_sdk::shape::ConfigShape::from_value(v)
                        .map_err(|e| e.under(#wire))?
                };
            });
        }
        binds.push(quote! { #fident });
    }

    let expanded = quote! {
        impl ::lattice_plugin_sdk::shape::ConfigShape for #ident {
            fn schema() -> ::lattice_plugin_sdk::shape::Schema {
                ::lattice_plugin_sdk::shape::Schema::Record(::std::vec![ #(#field_schema),* ])
            }

            fn to_value(&self) -> ::lattice_plugin_sdk::shape::Value {
                let mut fields: ::std::vec::Vec<(::std::string::String, ::lattice_plugin_sdk::shape::Value)> =
                    ::std::vec::Vec::new();
                #(#field_to)*
                ::lattice_plugin_sdk::shape::Value::record(fields)
            }

            fn from_value(
                value: &::lattice_plugin_sdk::shape::Value,
            ) -> ::core::result::Result<Self, ::lattice_plugin_sdk::shape::ShapeError> {
                if !::core::matches!(value, ::lattice_plugin_sdk::shape::Value::Record(_)) {
                    return ::core::result::Result::Err(
                        ::lattice_plugin_sdk::shape::ShapeError::new(::std::format!(
                            "expected record, got {}",
                            value.kind_label()
                        )),
                    );
                }
                #(#field_from)*
                ::core::result::Result::Ok(Self { #(#binds),* })
            }
        }
    };
    expanded.into()
}

fn config_shape_for_enum(ident: &syn::Ident, e: &syn::DataEnum) -> TokenStream {
    let mut forms = Vec::new();
    let mut to_arms = Vec::new();
    let mut from_arms = Vec::new();

    for v in &e.variants {
        if !matches!(v.fields, Fields::Unit) {
            return syn::Error::new_spanned(
                v,
                "`#[derive(ConfigShape)]` on an enum requires every variant to be a unit variant \
                 — a data-carrying variant has no `config-schema` arm, and flattening one \
                 silently would describe a shape the host then validates against wrongly",
            )
            .to_compile_error()
            .into();
        }
        let vident = &v.ident;
        let wire = to_kebab_case(vident.to_string().trim_start_matches("r#"));
        forms.push(quote! { #wire.to_string() });
        to_arms.push(quote! {
            Self::#vident => ::lattice_plugin_sdk::shape::Value::Str(#wire.to_string())
        });
        from_arms.push(quote! { #wire => ::core::result::Result::Ok(Self::#vident) });
    }

    let expanded = quote! {
        impl ::lattice_plugin_sdk::shape::ConfigShape for #ident {
            fn schema() -> ::lattice_plugin_sdk::shape::Schema {
                ::lattice_plugin_sdk::shape::Schema::Enum(::std::vec![ #(#forms),* ])
            }

            fn to_value(&self) -> ::lattice_plugin_sdk::shape::Value {
                match self { #(#to_arms),* }
            }

            fn from_value(
                value: &::lattice_plugin_sdk::shape::Value,
            ) -> ::core::result::Result<Self, ::lattice_plugin_sdk::shape::ShapeError> {
                let got = value.as_str().ok_or_else(|| {
                    ::lattice_plugin_sdk::shape::ShapeError::new(::std::format!(
                        "expected string, got {}",
                        value.kind_label()
                    ))
                })?;
                match got {
                    #(#from_arms,)*
                    // The valid set, inline. An enum's whole advantage over a
                    // free string is that the answer is finite, so a rejection
                    // that does not show it wastes the one thing it knows.
                    other => ::core::result::Result::Err(
                        ::lattice_plugin_sdk::shape::ShapeError::new(::std::format!(
                            "expected one of {}, got `{}`",
                            [ #(#forms),* ].join(" | "),
                            other
                        )),
                    ),
                }
            }
        }
    };
    expanded.into()
}

/// Whether `ty` is spelled `Option<...>` — how a field says it is not required.
///
/// Syntactic, by the last path segment, so `std::option::Option<T>` and a bare
/// `Option<T>` both match and a user type happening to be named `Option` would
/// too. That is the same trade `option_kind_for` makes for `bool` / `i64` /
/// `String`, and a proc macro has no types to ask.
fn is_option_type(ty: &Type) -> bool {
    if let Type::Path(p) = ty
        && let Some(seg) = p.path.segments.last()
    {
        return seg.ident == "Option"
            && matches!(seg.arguments, syn::PathArguments::AngleBracketed(_));
    }
    false
}
