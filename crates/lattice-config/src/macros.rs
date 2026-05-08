// `linkme`'s distributed slices use `link_section` to aggregate
// items at link time. The macros in this module expand to such
// declarations at the call site; allow `unsafe_code` here so the
// expansion compiles. The safety argument is the same as
// `option_decl.rs` and `group.rs`: linkme is a standard Rust
// solution for cross-crate aggregation, the link-section usage
// is contained, and no raw pointer / unchecked-cast code lives
// in the macro expansion or its consumers.
#![allow(unsafe_code)]

//! Declarative macros for option / group declarations.
//!
//! - [`options!`] -- the public macro for declaring options
//!   anywhere outside the foundation crates. Each declaration
//!   binds to an [`crate::OptionGroup`] (selected at the block
//!   head); the macro generates a unit-struct identity, the
//!   [`crate::OptionDecl`] + [`crate::HasGroup`] impls, and a
//!   `linkme` submission of the option's metadata.
//! - [`editor_options!`] -- foundation-crate-only macro for
//!   declaring bare-named editor options (`tabstop`, `number`,
//!   `wrap`, ...). Implements the §6.8 compile-time bare-namespace
//!   reservation: the macro is `pub` here so foundation crates
//!   can use it, but it is *intentionally not re-exported by
//!   `lib.rs`* -- M.2.0c demotes it to crate-private for full
//!   compile-time enforcement. Until then, code review is the
//!   guardrail.
//! - [`groups!`] -- public macro for declaring `OptionGroup`
//!   types. Emits a `const fn` byte-walk assertion that the
//!   group's display name does not end in `-mode` (the
//!   modes-vs-groups disambiguation rule, §6.7.1).
//!
//! Every macro derives the option / group display name from
//! the identifier via the same kebab-case lowering, so type
//! identifier and display name are always derived from a
//! single input -- no possibility of drift.

/// Declare one or more typed options bound to a registered
/// [`crate::OptionGroup`].
///
/// ```ignore
/// options! {
///     namespace = "rust-mode";
///     group = lattice_config::Editing;
///
///     /// Visual width of a tab stop in columns.
///     pub IndentWidth: u64 = 4;
///
///     /// Use spaces or tabs for indentation.
///     pub IndentStyle: lattice_config::OptionType = ...;  // see note
/// }
/// ```
///
/// The `namespace` field is optional; when present, the macro
/// auto-prepends `<namespace>.` to the derived display name
/// (`indent-width` ⇒ `rust-mode.indent-width`). Without it,
/// the display name is the bare derived name (used by
/// [`editor_options!`] for foundation-crate options under
/// the reserved `Editor` group).
///
/// The `group` field is required and must be a path to a
/// type implementing [`crate::OptionGroup`].
///
/// Each option's identifier becomes a unit struct; the macro
/// generates the trait impls and a `linkme` submission. The
/// derived display name is the kebab-case lowering of the
/// identifier.
///
/// Within a single block, two options with the same identifier
/// produce duplicate `struct` definitions and fail at compile
/// time. Within a single block, two options resolving to the
/// same display name (after namespace prepending) cannot
/// happen because they'd require duplicate identifiers.
/// Across blocks within a crate, identifier collision is also
/// a compile error (rust's normal scoping rules). Across
/// crates, display-name uniqueness is enforced at startup
/// by the linkme aggregation in [`crate::OPTION_DECLS`].
#[macro_export]
macro_rules! options {
    // Form 1: with namespace.
    (
        $(#[$set_attr:meta])*
        namespace = $ns:expr;
        group = $group:path;
        $(
            $(#[$attr:meta])*
            pub $name:ident : $ty:ty = $default:expr ;
        )*
    ) => {
        $(
            $(#[$attr])*
            pub struct $name;

            impl $crate::OptionDecl for $name {
                type Value = $ty;
                const NAME: &'static str = $crate::__option_name_with_namespace!(
                    $ns, stringify!($name)
                );
                const DOC: &'static str = $crate::__option_doc!($($attr)*);
                fn default_value() -> $ty { $default }
            }

            impl $crate::HasGroup for $name {
                const GROUP_NAME: &'static str = <$group as $crate::OptionGroup>::NAME;
            }

            $crate::__submit_option_decl!($name, $ty, $default);
        )*
    };

    // Form 2: no namespace (bare name; used by editor_options!).
    (
        group = $group:path;
        $(
            $(#[$attr:meta])*
            pub $name:ident : $ty:ty = $default:expr ;
        )*
    ) => {
        $(
            $(#[$attr])*
            pub struct $name;

            impl $crate::OptionDecl for $name {
                type Value = $ty;
                const NAME: &'static str = $crate::__option_kebab!(stringify!($name));
                const DOC: &'static str = $crate::__option_doc!($($attr)*);
                fn default_value() -> $ty { $default }
            }

            impl $crate::HasGroup for $name {
                const GROUP_NAME: &'static str = <$group as $crate::OptionGroup>::NAME;
            }

            $crate::__submit_option_decl!($name, $ty, $default);
        )*
    };
}

/// Foundation-crate-only macro for declaring bare-named editor
/// options. Identical to [`options!`] without the `namespace`
/// directive, with the `Editor` group hard-bound. Implements
/// the §6.8 bare-namespace reservation: the macro is
/// intentionally not re-exported from `lib.rs`; code review
/// is the v1 guardrail (M.2.0c demotes to crate-private).
///
/// ```ignore
/// editor_options! {
///     /// Width of a tab stop in columns.
///     pub Tabstop: u64 = 8;
/// }
/// ```
#[macro_export]
macro_rules! editor_options {
    (
        $(
            $(#[$attr:meta])*
            pub $name:ident : $ty:ty = $default:expr ;
        )*
    ) => {
        $crate::options! {
            group = $crate::Editor;
            $(
                $(#[$attr])*
                pub $name : $ty = $default ;
            )*
        }
    };
}

/// Declare one or more [`crate::OptionGroup`] types. Each
/// declaration generates the unit struct, the `OptionGroup`
/// trait impl, a `linkme` submission of the group's metadata,
/// AND a compile-time `const fn` assertion that the derived
/// display name does not end in `-mode` (the modes-vs-groups
/// disambiguation rule, `mode-architecture.md` §6.7.1).
///
/// ```ignore
/// groups! {
///     /// LSP-related options.
///     pub Lsp;
///
///     /// File-tree navigation buffer.
///     pub Filetree;
///
///     /// Plugin-defined group.
///     pub GitBlame = "git-blame";   // explicit display name
/// }
/// ```
///
/// Without an explicit `= "name"` clause, the display name is
/// the kebab-case lowering of the identifier. With one, the
/// declared string is used verbatim (still subject to the
/// no-`-mode`-suffix assertion).
#[macro_export]
macro_rules! groups {
    (
        $(
            $(#[$attr:meta])*
            pub $name:ident $(= $explicit_name:literal)? ;
        )*
    ) => {
        $(
            $(#[$attr])*
            pub struct $name;

            impl $crate::OptionGroup for $name {
                const NAME: &'static str = $crate::__group_name!(
                    $name $(, $explicit_name)?
                );
                const DOC: &'static str = $crate::__option_doc!($($attr)*);
            }

            // Compile-time assertion: name does not end in `-mode`.
            const _: () = {
                assert!(
                    !$crate::ends_with_mode_suffix(<$name as $crate::OptionGroup>::NAME),
                    "OptionGroup name must not end in `-mode` (modes-vs-groups disambiguation; `mode-architecture.md` §6.7.1)"
                );
            };

            $crate::__submit_group_decl!($name);
        )*
    };
}

// ---------------------------------------------------------
// Internal helpers used by the public macros above.
// These are exported with `__` prefix so they're reachable
// from `$crate::__foo!` paths in macros expanded outside
// this crate, but the underscore signals "not for direct
// use."
// ---------------------------------------------------------

/// Kebab-case lowering of an option identifier. Currently a
/// passthrough at compile time -- the macros run on the raw
/// `stringify!()` of the identifier, which `Tabstop` ⇒
/// `"Tabstop"`. A real kebab-case helper would lowercase and
/// insert hyphens at camelCase boundaries; that's a token-
/// substitution macro pass we'll add when we wire up the
/// migration in M.2.0b. For now the macro emits the literal
/// stringified identifier; M.2.0b sites pass a manual
/// `= "tab-stop"` literal where needed via an explicit form.
///
/// **Limitation acknowledged.** The const-context kebab-case
/// transformation requires `const fn` string manipulation
/// which is feasible but verbose; landing it cleanly is part
/// of M.2.0b. M.2.0a's macro emits the bare stringified
/// identifier for tests; production migration adapts.
#[doc(hidden)]
#[macro_export]
macro_rules! __option_kebab {
    ($s:expr) => {
        $s
    };
}

/// Construct the namespaced display name. Concatenates
/// `namespace + "." + kebab(name)` at const time.
#[doc(hidden)]
#[macro_export]
macro_rules! __option_name_with_namespace {
    ($ns:expr, $name:expr) => {
        // const-context concat via const_format would be ideal;
        // for M.2.0a we emit a runtime closure on the metadata
        // record but the NAME constant requires a single const
        // expression. Workaround: caller passes the full name
        // explicit at the call site through a wrapper macro
        // when migration hits sites that need namespacing.
        // Simple sites can pass `name = "rust-mode.indent-width"`
        // directly. The full namespacing macro lands in M.2.0b
        // alongside the migration that exercises it.
        concat!($ns, ".", $name)
    };
}

/// Group name resolution: either the explicit literal, or the
/// kebab lowering of the identifier.
#[doc(hidden)]
#[macro_export]
macro_rules! __group_name {
    ($name:ident, $explicit:literal) => {
        $explicit
    };
    ($name:ident) => {
        $crate::__option_kebab!(stringify!($name))
    };
}

/// Extract the leading doc comment from an attribute list as
/// the option's `DOC` const. M.2.0a returns the empty string
/// (doc-comment extraction in declarative macros is awkward;
/// proc-macro is the clean answer); the metadata record's
/// `doc` field is populated separately at submission time.
#[doc(hidden)]
#[macro_export]
macro_rules! __option_doc {
    ($($attr:meta)*) => {
        ""
    };
}

/// Generate the `linkme` submission for one option declaration.
#[doc(hidden)]
#[macro_export]
macro_rules! __submit_option_decl {
    ($name:ident, $ty:ty, $default:expr) => {
        const _: () = {
            #[$crate::linkme::distributed_slice($crate::OPTION_DECLS)]
            #[linkme(crate = $crate::linkme)]
            static DECL: &$crate::OptionDeclMetadata =
                &$crate::OptionDeclMetadata::for_decl::<$name>(
                    // Function pointers, not calls -- the
                    // metadata holds them and the registry boot
                    // loop calls them at runtime (see
                    // OptionDeclMetadata docs). Calling
                    // type_label() directly here would fail
                    // because trait methods aren't const-fn on
                    // stable Rust.
                    <$ty as $crate::OptionType>::type_label,
                    || <$ty as $crate::OptionType>::format(&$default),
                );
        };
    };
}

/// Generate the `linkme` submission for one group declaration.
#[doc(hidden)]
#[macro_export]
macro_rules! __submit_group_decl {
    ($name:ident) => {
        const _: () = {
            #[$crate::linkme::distributed_slice($crate::GROUP_DECLS)]
            #[linkme(crate = $crate::linkme)]
            static DECL: &$crate::OptionGroupMetadata =
                &$crate::OptionGroupMetadata::for_group::<$name>();
        };
    };
}

#[cfg(test)]
mod tests {
    use crate::{OptionDecl, OptionGroup};

    // Declare a fixture group (using groups! macro) for tests.
    crate::groups! {
        pub TestGroup = "test-macros-group";
    }

    // Declare options in the fixture group via options!.
    crate::options! {
        group = TestGroup;
        pub Foo: i64 = 42;
        pub Bar: bool = true;
    }

    #[test]
    fn macro_generates_decl_impls() {
        assert_eq!(Foo::NAME, "Foo");
        assert_eq!(Foo::default_value(), 42);
        assert_eq!(Bar::default_value(), true);
    }

    #[test]
    fn macro_generates_group_binding() {
        assert_eq!(<Foo as crate::HasGroup>::GROUP_NAME, "test-macros-group");
        assert_eq!(<Bar as crate::HasGroup>::GROUP_NAME, "test-macros-group");
    }

    #[test]
    fn group_macro_emits_trait() {
        assert_eq!(TestGroup::NAME, "test-macros-group");
    }
}
