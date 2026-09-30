//! `labeled_enum!` — declarative macro for enum-typed options that
//! participate in `:set foo=<Tab>` cmdline completion.
//!
//! Generates the enum together with four colocated accessors:
//!
//!   - `label()`     — canonical string form, used by
//!     `:set foo=...` parsing + the `:set foo?` echo.
//!   - `parse_label` — string → variant (accepts the canonical
//!     form + any registered aliases per variant).
//!   - `doc()`       — short marginalia doc shown in the
//!     completion popup's right-aligned column.
//!   - `all()`       — variants in declaration order (drives
//!     `:set foo=<Tab>` enumeration).
//!
//! Single source of truth: each variant's label and doc are
//! declared together. Adding a new variant requires one new line;
//! the macro extends every accessor in lockstep.
//!
//! ## Syntax
//!
//! ```
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
//!
//! assert_eq!(FoldMethod::default(), FoldMethod::Manual);
//! assert_eq!(FoldMethod::Indent.label(), "indent");
//! assert_eq!(FoldMethod::parse_label("markdown"), Ok(FoldMethod::Markdown));
//! assert_eq!(FoldMethod::all().len(), 3);
//! ```
//!
//! ### Aliases
//!
//! A variant can accept multiple parse forms; the first is the
//! canonical label, the rest are aliases:
//!
//! ```
//! # use lattice_core::labeled_enum;
//! labeled_enum! {
//!     /// Where a produced buffer is displayed.
//!     pub enum Placement {
//!         /// Built-in default.
//!         #[default]
//!         Default = "default" => "Use the category's built-in default",
//!         /// Centred popup.
//!         PopupCentered = "popup-centered" | "popup" => "Centred focused popup",
//!         /// Hover-style popup.
//!         FloatingCursor = "floating-cursor" | "floating" => "Floating popup",
//!     }
//! }
//!
//! // The alias parses to the same variant …
//! assert_eq!(Placement::parse_label("popup"), Ok(Placement::PopupCentered));
//! // … but only canonical labels are enumerated and echoed.
//! assert_eq!(Placement::PopupCentered.label(), "popup-centered");
//! assert_eq!(
//!     Placement::all().iter().map(|p| p.label()).collect::<Vec<_>>(),
//!     ["default", "popup-centered", "floating-cursor"],
//! );
//! // Unknown input lists the canonical forms.
//! assert_eq!(
//!     Placement::parse_label("pop"),
//!     Err("expected `default`, `popup-centered`, or `floating-cursor`, got `pop`".to_string()),
//! );
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
//! ```
//! # use lattice_core::labeled_enum;
//! labeled_enum! {
//!     /// Log verbosity.
//!     #[derive(Hash)]
//!     pub enum LogLevel {
//!         /// Errors only.
//!         #[default]
//!         Error = "error" => "Errors only",
//!         /// Everything.
//!         Debug = "debug" => "Everything",
//!     }
//! }
//!
//! let set: std::collections::HashSet<LogLevel> = LogLevel::all().iter().copied().collect();
//! assert!(set.contains(&LogLevel::Debug));
//! ```
//!
//! Every variant, and the enum itself, should carry a `///` doc: the
//! macro forwards attributes, so an undocumented variant trips
//! `missing_docs` in a crate that has opted into it.

/// Declare an option enum whose variants carry a canonical label, optional
/// parse aliases and a one-line completion doc.
///
/// Expands to the enum (deriving `Debug, Clone, Copy, PartialEq, Eq,
/// Default`) plus inherent `label()`, `doc()`, `all()` and
/// `parse_label()`. Exactly one variant must be `#[default]`. See the
/// [module docs](mod@crate::labeled_enum) for syntax, aliases and extra derives.
///
/// # Examples
///
/// ```
/// use lattice_core::labeled_enum;
///
/// labeled_enum! {
///     /// `:set bell=...`.
///     pub enum Bell {
///         /// Silent.
///         #[default]
///         Off = "off" | "false" => "No bell",
///         /// Flash the screen.
///         Visual = "visual" => "Flash instead of beeping",
///     }
/// }
///
/// assert_eq!(Bell::parse_label("false"), Ok(Bell::Off));
/// assert_eq!(Bell::Visual.doc(), "Flash instead of beeping");
/// assert!(Bell::parse_label("loud").is_err());
/// ```
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
