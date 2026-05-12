//! Proc-macro front-end for `lattice-config`'s declarative
//! option / group / overrides declarations (M.2.0+M.2.1, Design
//! B + D from `docs/dev/architecture/mode-architecture.md` discussion notes).
//!
//! Three public macros:
//!
//! - `options!` — declares one or more typed options, each
//!   bound to a registered `OptionGroup`. Supports
//!   doc-comment capture, `#[aliases("...")]`,
//!   `#[validate(fn_path)]`, `#[name("...")]` (explicit
//!   display-name override), and `#[customizable(bool)]`.
//!   Self-registers each option via `linkme::distributed_slice`
//!   so the registry's pre-`main` boot walks the slice and
//!   inserts every option without a central
//!   `register_core_options` call.
//!
//! - `groups!` — declares one or more `OptionGroup` types.
//!   Emits a compile-time `const fn` byte-walk assertion that
//!   each group's display name does NOT end in `-mode` (the
//!   modes-vs-groups disambiguation rule, `mode-architecture.md`
//!   §6.7.1).
//!
//! - `overrides!` — constructs a typed `OptionOverrideSet`
//!   from a list of `OptionDecl = value` pairs (M.2.1).
//!   Compile-time-checks that each value matches its
//!   declaration's `Value` type via a let-ascription
//!   intermediate. Optional `#[priority(High)]` /
//!   `#[priority(Low)]` attribute promotes individual entries.
//!   Used by `Mode::options()` impls.
//!
//! Both macros emit code that references absolute paths into
//! `::lattice_config`; the consumer crate must depend on
//! `lattice-config`. The host crate (`lattice-config` itself)
//! uses `extern crate self as lattice_config;` so the same
//! absolute paths resolve in its own code.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Attribute, Expr, ExprLit, Ident, Lit, LitStr, Path, Token, Type,
    parse::{Parse, ParseStream},
    parse_macro_input,
    punctuated::Punctuated,
};

// ---------------------------------------------------------
// `options!` proc macro
// ---------------------------------------------------------

/// Declarative option declaration block. See the trait doc on
/// `lattice_config::OptionDecl` and the macro doc on
/// `lattice_config::options!` for the surface contract.
#[proc_macro]
pub fn options(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as OptionsInput);
    match expand_options(&parsed) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

struct OptionsInput {
    namespace: Option<LitStr>,
    group: Path,
    decls: Vec<OptionDecl>,
}

struct OptionDecl {
    attrs: Vec<Attribute>,
    name: Ident,
    ty: Type,
    default: Expr,
}

impl Parse for OptionsInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        // Optional `namespace = "..."`;
        let namespace = if peek_keyword(input, "namespace") {
            let _: Ident = input.parse()?;
            let _: Token![=] = input.parse()?;
            let s: LitStr = input.parse()?;
            let _: Token![;] = input.parse()?;
            Some(s)
        } else {
            None
        };

        // Required `group = path`;
        let group_kw: Ident = input.parse()?;
        if group_kw != "group" {
            return Err(syn::Error::new(
                group_kw.span(),
                "expected `group = <path>;` directive",
            ));
        }
        let _: Token![=] = input.parse()?;
        let group: Path = input.parse()?;
        let _: Token![;] = input.parse()?;

        // Declarations until end of input.
        let mut decls = Vec::new();
        while !input.is_empty() {
            let attrs = input.call(Attribute::parse_outer)?;
            let _: Token![pub] = input.parse()?;
            let name: Ident = input.parse()?;
            let _: Token![:] = input.parse()?;
            let ty: Type = input.parse()?;
            let _: Token![=] = input.parse()?;
            let default: Expr = input.parse()?;
            let _: Token![;] = input.parse()?;
            decls.push(OptionDecl {
                attrs,
                name,
                ty,
                default,
            });
        }

        Ok(Self {
            namespace,
            group,
            decls,
        })
    }
}

fn peek_keyword(input: ParseStream, name: &str) -> bool {
    let fork = input.fork();
    if let Ok(id) = fork.parse::<Ident>() {
        id == name
    } else {
        false
    }
}

fn expand_options(input: &OptionsInput) -> syn::Result<TokenStream2> {
    let mut out = TokenStream2::new();
    for decl in &input.decls {
        out.extend(expand_one_option(
            decl,
            &input.group,
            input.namespace.as_ref(),
        )?);
    }
    Ok(out)
}

fn expand_one_option(
    decl: &OptionDecl,
    group: &Path,
    namespace: Option<&LitStr>,
) -> syn::Result<TokenStream2> {
    let name_ident = &decl.name;
    let ty = &decl.ty;
    let default = &decl.default;

    // Extract the option's facts from attribute list.
    let facts = OptionFacts::extract(&decl.attrs)?;

    // Compute display name.
    let display_name = match &facts.explicit_name {
        Some(s) => s.value(),
        None => {
            let kebab = camel_to_kebab(&name_ident.to_string());
            match namespace {
                Some(ns) => format!("{}.{}", ns.value(), kebab),
                None => kebab,
            }
        }
    };

    let doc = facts.doc;
    let aliases = &facts.aliases;
    let validate = facts.validate.as_ref();
    let customizable = facts.customizable;

    // Build-spec function: constructs the runtime `Option<T>`
    // spec by calling the existing builder API. The macro is
    // a builder front-end (Design B from the design notes);
    // the registration boot calls this and forwards to the
    // registry's `register` path.
    let aliases_lit: Vec<LitStr> = aliases
        .iter()
        .map(|s| LitStr::new(&s.value(), s.span()))
        .collect();
    let aliases_slice = if aliases_lit.is_empty() {
        quote! { &[] as &[&'static str] }
    } else {
        quote! { &[#(#aliases_lit),*] }
    };
    let validate_chain = match validate {
        Some(path) => quote! { .validate(#path) },
        None => quote! {},
    };

    let display_name_lit = LitStr::new(&display_name, name_ident.span());
    let doc_lit = LitStr::new(&doc, name_ident.span());

    // A unique-per-option static identifier for the linkme
    // submission. Using `format_ident!` keeps it inside the
    // declaring crate's identifier space; collisions can't
    // happen because the source identifier is itself unique
    // in the local scope.
    let link_name = format_ident!("__LATTICE_OPTION_DECL_{}", uppercase(&name_ident.to_string()));

    Ok(quote! {
        // ---- Type identity ----
        #[doc = #doc_lit]
        pub struct #name_ident;

        impl ::lattice_config::OptionDecl for #name_ident {
            type Value = #ty;
            const NAME: &'static str = #display_name_lit;
            const DOC: &'static str = #doc_lit;
            const CUSTOMIZABLE: bool = #customizable;
            fn default_value() -> Self::Value {
                #default
            }
        }

        impl ::lattice_config::HasGroup for #name_ident {
            const GROUP_NAME: &'static str =
                <#group as ::lattice_config::OptionGroup>::NAME;
        }

        impl #name_ident {
            /// Build the runtime spec for this declaration.
            /// Used by the registry's self-registration boot
            /// path; not generally called by hand. Direct
            /// construction of `Option<T>` is the macro's
            /// internal-only path -- consumer code uses
            /// `config.get_typed::<#name_ident>()` to read
            /// and `config.set_typed::<#name_ident>(...)` to
            /// write.
            pub fn build_spec() -> ::lattice_config::option::Option<#ty> {
                let b = ::lattice_config::option::Option::<#ty>::builder(
                    #display_name_lit,
                    #default,
                    #doc_lit,
                );
                #[allow(clippy::let_and_return)]
                let b = b.aliases(#aliases_slice);
                let b = b #validate_chain;
                b.build()
            }
        }

        // ---- linkme submission for self-registration ----
        //
        // The `register_fn` thunk is called by the registry's
        // `init_from_linkme` boot. It constructs the runtime
        // spec via the macro-generated `build_spec()` and
        // registers it with the typed-id mapping so type-keyed
        // reads (`config.get_typed::<Self>()`) work post-boot.
        const _: () = {
            #[::lattice_config::linkme::distributed_slice(
                ::lattice_config::OPTION_DECLS
            )]
            #[linkme(crate = ::lattice_config::linkme)]
            static #link_name: &::lattice_config::OptionDeclMetadata =
                &::lattice_config::OptionDeclMetadata::for_decl::<#name_ident>(
                    <#ty as ::lattice_config::OptionType>::type_label,
                    || <#ty as ::lattice_config::OptionType>::format(&#default),
                    |registry: &::lattice_config::ConfigRegistry| {
                        let _ = registry.register_with_typeid::<#ty>(
                            <#name_ident>::build_spec(),
                            ::std::any::TypeId::of::<#name_ident>(),
                        );
                    },
                );
        };
    })
}

#[derive(Default)]
struct OptionFacts {
    doc: String,
    aliases: Vec<LitStr>,
    validate: Option<Path>,
    explicit_name: Option<LitStr>,
    customizable: bool,
}

impl OptionFacts {
    fn extract(attrs: &[Attribute]) -> syn::Result<Self> {
        let mut facts = Self {
            customizable: true,
            ..Self::default()
        };
        let mut doc_lines: Vec<String> = Vec::new();
        for attr in attrs {
            if attr.path().is_ident("doc") {
                if let syn::Meta::NameValue(nv) = &attr.meta
                    && let Expr::Lit(ExprLit {
                        lit: Lit::Str(s), ..
                    }) = &nv.value
                {
                    let line = s.value();
                    let trimmed = line.strip_prefix(' ').unwrap_or(&line);
                    doc_lines.push(trimmed.to_string());
                }
            } else if attr.path().is_ident("aliases") {
                let parsed: Punctuated<LitStr, Token![,]> =
                    attr.parse_args_with(Punctuated::parse_terminated)?;
                for s in parsed {
                    facts.aliases.push(s);
                }
            } else if attr.path().is_ident("validate") {
                let p: Path = attr.parse_args()?;
                facts.validate = Some(p);
            } else if attr.path().is_ident("name") {
                let s: LitStr = attr.parse_args()?;
                facts.explicit_name = Some(s);
            } else if attr.path().is_ident("customizable") {
                let b: syn::LitBool = attr.parse_args()?;
                facts.customizable = b.value;
            }
            // Other attributes pass through silently -- e.g.
            // `#[cfg(...)]` on declarations is a future feature.
        }
        facts.doc = doc_lines.join("\n");
        Ok(facts)
    }
}

// ---------------------------------------------------------
// `groups!` proc macro
// ---------------------------------------------------------

/// Declarative group declaration block. See
/// `lattice_config::OptionGroup` and `lattice_config::groups!`
/// for the surface contract.
#[proc_macro]
pub fn groups(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as GroupsInput);
    match expand_groups(&parsed) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

struct GroupsInput {
    decls: Vec<GroupDecl>,
}

struct GroupDecl {
    attrs: Vec<Attribute>,
    name: Ident,
    explicit_name: Option<LitStr>,
}

impl Parse for GroupsInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut decls = Vec::new();
        while !input.is_empty() {
            let attrs = input.call(Attribute::parse_outer)?;
            let _: Token![pub] = input.parse()?;
            let name: Ident = input.parse()?;
            let explicit_name = if input.peek(Token![=]) {
                let _: Token![=] = input.parse()?;
                Some(input.parse::<LitStr>()?)
            } else {
                None
            };
            let _: Token![;] = input.parse()?;
            decls.push(GroupDecl {
                attrs,
                name,
                explicit_name,
            });
        }
        Ok(Self { decls })
    }
}

fn expand_groups(input: &GroupsInput) -> syn::Result<TokenStream2> {
    let mut out = TokenStream2::new();
    for decl in &input.decls {
        out.extend(expand_one_group(decl)?);
    }
    Ok(out)
}

fn expand_one_group(decl: &GroupDecl) -> syn::Result<TokenStream2> {
    let name_ident = &decl.name;
    let display_name = match &decl.explicit_name {
        Some(s) => s.value(),
        None => camel_to_kebab(&name_ident.to_string()),
    };

    // Doc collection.
    let mut doc_lines: Vec<String> = Vec::new();
    for attr in &decl.attrs {
        if attr.path().is_ident("doc")
            && let syn::Meta::NameValue(nv) = &attr.meta
            && let Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) = &nv.value
        {
            let line = s.value();
            let trimmed = line.strip_prefix(' ').unwrap_or(&line);
            doc_lines.push(trimmed.to_string());
        }
    }
    let doc = doc_lines.join("\n");

    let display_name_lit = LitStr::new(&display_name, name_ident.span());
    let doc_lit = LitStr::new(&doc, name_ident.span());
    let link_name = format_ident!("__LATTICE_GROUP_DECL_{}", uppercase(&name_ident.to_string()));

    Ok(quote! {
        #[doc = #doc_lit]
        pub struct #name_ident;

        impl ::lattice_config::OptionGroup for #name_ident {
            const NAME: &'static str = #display_name_lit;
            const DOC: &'static str = #doc_lit;
        }

        // Compile-time assertion: name does NOT end in `-mode`.
        const _: () = {
            assert!(
                !::lattice_config::ends_with_mode_suffix(
                    <#name_ident as ::lattice_config::OptionGroup>::NAME
                ),
                "OptionGroup name must not end in `-mode` \
                 (modes-vs-groups disambiguation; \
                 `mode-architecture.md` §6.7.1)"
            );
        };

        const _: () = {
            #[::lattice_config::linkme::distributed_slice(
                ::lattice_config::GROUP_DECLS
            )]
            #[linkme(crate = ::lattice_config::linkme)]
            static #link_name: &::lattice_config::OptionGroupMetadata =
                &::lattice_config::OptionGroupMetadata::for_group::<#name_ident>();
        };
    })
}

// ---------------------------------------------------------
// `overrides!` proc macro
// ---------------------------------------------------------

/// Construct a typed `OptionOverrideSet` for a `Mode::options()`
/// return.
///
/// Compile-time-typed: each entry is a path to an
/// `OptionDecl` followed by `= value`. The macro emits a
/// let-binding ascribed to `<#decl as OptionDecl>::Value` so
/// values that don't match the declaration's `Value` type are
/// compile errors. Optional `#[priority(High)]` /
/// `#[priority(Low)]` attribute on an entry promotes it.
///
/// ```ignore
/// fn options(&self) -> OptionOverrideSet {
///     lattice_config::overrides! {
///         Tabstop = 4,
///         IndentStyle = IndentStyle::Spaces,
///         #[priority(High)]
///         ReadOnly = true,
///     }
/// }
/// ```
#[proc_macro]
pub fn overrides(input: TokenStream) -> TokenStream {
    let parsed = parse_macro_input!(input as OverridesInput);
    match expand_overrides(&parsed) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

struct OverridesInput {
    entries: Vec<OverrideEntry>,
}

struct OverrideEntry {
    /// Optional `#[priority(...)]` attribute.
    priority: Option<Ident>,
    decl: Path,
    value: Expr,
}

impl Parse for OverridesInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut entries = Vec::new();
        while !input.is_empty() {
            // Optional outer attributes (only `#[priority(...)]`
            // is currently meaningful; others silently pass).
            let attrs = input.call(Attribute::parse_outer)?;
            let priority = parse_priority(&attrs)?;
            let decl: Path = input.parse()?;
            let _: Token![=] = input.parse()?;
            let value: Expr = input.parse()?;
            // Optional trailing comma.
            if !input.is_empty() {
                let _: Token![,] = input.parse()?;
            }
            entries.push(OverrideEntry {
                priority,
                decl,
                value,
            });
        }
        Ok(Self { entries })
    }
}

fn parse_priority(attrs: &[Attribute]) -> syn::Result<Option<Ident>> {
    for attr in attrs {
        if attr.path().is_ident("priority") {
            let ident: Ident = attr.parse_args()?;
            // Validate the identifier is one we recognise.
            if ident != "High" && ident != "Low" && ident != "Normal" {
                return Err(syn::Error::new(
                    ident.span(),
                    "expected `High`, `Low`, or `Normal`",
                ));
            }
            return Ok(Some(ident));
        }
    }
    Ok(None)
}

fn expand_overrides(input: &OverridesInput) -> syn::Result<TokenStream2> {
    let pushes: Vec<TokenStream2> = input
        .entries
        .iter()
        .map(|entry| {
            let decl = &entry.decl;
            let value = &entry.value;
            if let Some(priority) = &entry.priority {
                quote! {
                    {
                        // Compile-time type check: the value
                        // must coerce to the declaration's
                        // Value type. Mismatches produce a
                        // legible compile error at this site.
                        let __value: <#decl as ::lattice_config::OptionDecl>::Value = #value;
                        __set.push(
                            ::lattice_config::OptionOverride::with_priority(
                                ::std::any::TypeId::of::<#decl>(),
                                __value,
                                ::lattice_config::OverridePriority::#priority,
                            )
                        );
                    }
                }
            } else {
                quote! {
                    {
                        let __value: <#decl as ::lattice_config::OptionDecl>::Value = #value;
                        __set.push(
                            ::lattice_config::OptionOverride::new(
                                ::std::any::TypeId::of::<#decl>(),
                                __value,
                            )
                        );
                    }
                }
            }
        })
        .collect();

    Ok(quote! {{
        let mut __set = ::lattice_config::OptionOverrideSet::new();
        #(#pushes)*
        __set
    }})
}

// ---------------------------------------------------------
// String helpers
// ---------------------------------------------------------

/// Convert a Rust identifier in PascalCase or camelCase to
/// kebab-case. E.g. `Tabstop` → `tabstop`, `RelativeNumber`
/// → `relative-number`, `CompletionGhostText` →
/// `completion-ghost-text`.
///
/// The transformation is byte-level on ASCII identifiers
/// (which Rust's identifier syntax permits beyond ASCII via
/// XID_Start / XID_Continue, but the option-name namespace
/// constrains us to ASCII anyway). Any character that's
/// already lowercase / digit / hyphen passes through; an
/// uppercase character introduces a hyphen separator before
/// itself (unless at position 0) and is lowercased. Other
/// characters (`_`) become hyphens.
fn camel_to_kebab(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            // Insert a separator before the uppercase letter
            // unless we're at the start OR the previous output
            // is already a hyphen (avoid `auto--insert` from
            // `Auto_Insert`).
            if i != 0 && !out.ends_with('-') {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else if c == '_' {
            // Underscore-to-hyphen transformation; collapse if
            // we just emitted one.
            if !out.ends_with('-') {
                out.push('-');
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn uppercase(s: &str) -> String {
    s.to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::camel_to_kebab;

    #[test]
    fn kebab_basic() {
        assert_eq!(camel_to_kebab("Tabstop"), "tabstop");
        assert_eq!(camel_to_kebab("RelativeNumber"), "relative-number");
        assert_eq!(camel_to_kebab("CompletionGhostText"), "completion-ghost-text");
        assert_eq!(camel_to_kebab("Number"), "number");
    }

    #[test]
    fn kebab_with_underscore() {
        // Underscore-separated names are accepted (a stylistic
        // alternative when PascalCase doesn't read well).
        assert_eq!(camel_to_kebab("Auto_Insert"), "auto-insert");
    }

    #[test]
    fn kebab_acronyms() {
        // Acronyms (LSP, UI) lower to runs of single-char
        // hyphenated segments. Acceptable trade-off for a
        // simple transformation; explicit `#[name("lsp")]`
        // overrides for the few sites that care.
        assert_eq!(camel_to_kebab("LSP"), "l-s-p");
        assert_eq!(camel_to_kebab("UIRender"), "u-i-render");
    }
}
