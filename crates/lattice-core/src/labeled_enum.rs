//! `labeled_enum!` — declarative macro for enum-typed options that
//! participate in `:set foo=<Tab>` cmdline completion.
//!
//! Generates the enum together with four colocated accessors:
//!
//!   - `label()`     — canonical string form, used by
//!                     `:set foo=...` parsing + the `:set foo?` echo.
//!   - `parse_label` — string → variant (accepts the canonical
//!                     form + any registered aliases per variant).
//!   - `doc()`       — short marginalia doc shown in the
//!                     completion popup's right-aligned column.
//!   - `all()`       — variants in declaration order (drives
//!                     `:set foo=<Tab>` enumeration).
//!
//! Single source of truth: each variant's label and doc are
//! declared together. Adding a new variant requires one new line;
//! the macro extends every accessor in lockstep.
//!
//! ## Syntax
//!
//! ```ignore
//! use lattice_core::labeled_enum;
//!
//! labeled_enum! {
//!     /// `:set foldmethod=...` — decides which provider feeds
//!     /// the per-buffer fold list.
//!     pub enum FoldMethod {
//!         /// `manual` — only user `zf` ranges.
//!         #[default]
//!         Manual = "manual" => "User-defined folds only (zf, zd)",
//!         /// `indent` — universal indent walker.
//!         Indent = "indent" => "Fold by indent level",
//!         /// `markdown` — ATX heading nesting.
//!         Markdown = "markdown" => "Fold by markdown headings",
//!     }
//! }
//! ```
//!
//! ### Aliases
//!
//! A variant can accept multiple parse forms; the first is the
//! canonical label, the rest are aliases:
//!
//! ```ignore
//! labeled_enum! {
//!     pub enum BufferDisplayPreference {
//!         #[default]
//!         Default = "default" => "Use the category's built-in default",
//!         PopupCentered = "popup-centered" | "popup" => "Centred focused popup",
//!         FloatingCursor = "floating-cursor" | "floating" => "...",
//!     }
//! }
//! ```
//!
//! Aliases parse to the same variant but DON'T appear in `all()` /
//! completion (only the canonical does).
//!
//! ### Derives
//!
//! The macro derives `Debug, Clone, Copy, PartialEq, Eq, Default`
//! on the enum. Exactly one variant must carry `#[default]`. Add
//! extra derives by stacking `#[derive(...)]` attributes BEFORE
//! `pub enum`:
//!
//! ```ignore
//! labeled_enum! {
//!     #[derive(Hash)]
//!     pub enum LogLevel { ... }
//! }
//! ```

/// See module docs.
#[macro_export]
macro_rules! labeled_enum {
    (
        $(#[$enum_attr:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_attr:meta])*
                $variant:ident = $canonical:literal $(| $alias:literal)* => $doc:literal
            ),* $(,)?
        }
    ) => {
        $(#[$enum_attr])*
        #[derive(
            ::std::fmt::Debug,
            ::std::clone::Clone,
            ::std::marker::Copy,
            ::std::cmp::PartialEq,
            ::std::cmp::Eq,
            ::std::default::Default,
        )]
        $vis enum $name {
            $(
                $(#[$variant_attr])*
                $variant,
            )*
        }

        impl $name {
            /// Canonical string label.
            pub fn label(self) -> &'static str {
                match self {
                    $( Self::$variant => $canonical, )*
                }
            }

            /// Short marginalia doc shown in cmdline-completion's
            /// right-aligned column.
            pub fn doc(self) -> &'static str {
                match self {
                    $( Self::$variant => $doc, )*
                }
            }

            /// Variants in declaration order.
            pub fn all() -> &'static [Self] {
                &[ $( Self::$variant ),* ]
            }

            /// Parse from canonical label or any registered alias.
            pub fn parse_label(s: &str) -> ::std::result::Result<Self, ::std::string::String> {
                match s {
                    $(
                        $canonical $( | $alias )* => Ok(Self::$variant),
                    )*
                    other => {
                        // Build "expected `a`, `b`, or `c`, got `x`"
                        // — Oxford comma, single-or before the last
                        // canonical. Matches the prior hand-written
                        // wording so user-visible error text doesn't
                        // drift on the macro migration.
                        let canonicals = [ $( $canonical ),* ];
                        let expected = match canonicals.as_slice() {
                            [] => ::std::string::String::new(),
                            [only] => format!("`{only}`"),
                            [a, b] => format!("`{a}` or `{b}`"),
                            many => {
                                let (last, rest) = many.split_last().unwrap();
                                let rest_quoted = rest
                                    .iter()
                                    .map(|s| format!("`{s}`"))
                                    .collect::<::std::vec::Vec<_>>()
                                    .join(", ");
                                format!("{rest_quoted}, or `{last}`")
                            }
                        };
                        Err(format!("expected {expected}, got `{other}`"))
                    }
                }
            }
        }
    };
}
