//! Typed configuration registry (DESIGN.md §5.12).
//!
//! Renderer-agnostic option machinery: the [`OptionType`] trait,
//! the typed [`Option<T>`] spec, the type-erased [`ErasedOption`]
//! trait the registry stores, and [`ConfigRegistry`] itself.
//!
//! ## Design (γ — value-on-spec storage)
//!
//! Each [`crate::option::Option<T>`] owns its current value behind an
//! [`arc_swap::ArcSwap<T>`]. Reads through a typed
//! [`crate::option::OptionHandle<T>`] are wait-free pointer loads. Writes go
//! through the registry (typed via `set` / by-name via
//! `parse_and_set_command`), which validates and stores.
//!
//! Renderer-specific options live in the renderer's crate but
//! register against the same [`ConfigRegistry`] at App startup.
//! The crate has no knowledge of the App or any concrete renderer.
//!
//! ## Crate boundary
//!
//! The trait + the four primitive impls (`bool`, `i64`, `String`,
//! plus this crate's `Color` when the foreign-type problem is
//! sorted) live here. Domain enums (`FoldMethod`, ...) implement
//! [`OptionType`] from their owning crate, importing the trait
//! from `lattice-config`.
//!
//! ## What's NOT here
//!
//! - **`App` reference.** Setters don't take an `&mut App`. The
//!   value lives in the spec; consumers read it through their
//!   typed handle. Renderers run side-effect cascades
//!   (`relativenumber` ⇒ `number`, `foldmethod` ⇒ recompute folds,
//!   `ui.*` ⇒ refresh derived theme styles) in their own
//!   post-set hook, polling the parsed `:set` form.
//! - **Renderer-specific options.** Options like `ui.separator` /
//!   `ui.statusline_active_fg` register from each renderer crate
//!   through the same `ConfigRegistry::register::<T>(...)` API.
//!   See `lattice-ui-tui::tui_options` for the TUI's example.
//! ## Event-bus integration (DESIGN.md §5.10 + §5.12)
//!
//! The registry optionally publishes [`lattice_protocol::Event::OptionChanged`]
//! on every successful set so consumers can react to typed-option
//! changes without polling. Wire it via
//! [`ConfigRegistry::set_event_publisher`] -- the closure receives
//! the `Event` and delegates to the consumer's bus. The crate is
//! agnostic to *which* bus (avoids a dep on `lattice-runtime`); the
//! App today calls `event_bus.publish(event)` from inside the
//! closure.
//!
//! Events fire on:
//! - typed `set::<T>(handle, value)` writes
//! - cmdline `parse_and_set_command(":set foo=bar")` (Assign)
//! - cmdline `:set nofoo` (Negate)
//! - cmdline `:set foo` boolean toggle (NameOnly on bool option)
//!
//! Events do NOT fire on `:set foo?` (Query) or on validation /
//! parse failures.

// Allow this crate to refer to itself by name. The
// `lattice-config-macros` proc macros emit code that
// references `::lattice_config::*` -- the absolute path lets
// expansions work uniformly in consumer crates AND inside
// `lattice-config` itself. Without this `extern crate`,
// `::lattice_config` doesn't resolve when the macro is
// invoked inside this crate.
extern crate self as lattice_config;

pub mod completion;
pub mod core_options;
mod diagnostics_options;
mod domain;
mod erased;
pub mod group;
pub mod loader;
mod modeline_zone;
mod signcolumn;
// `option` is `pub` so the proc macros' generated code can name
// `::lattice_config::option::Option<T>` for runtime spec
// construction. Direct construction of `Option<T>` is the
// macro-internal path (the macros' `build_spec()` calls
// `Option::<T>::builder(...)`); consumer-level code uses the
// macro and `config.get_typed::<X>()` instead.
pub mod option;
mod option_decl;
mod option_type;
mod origin;
// M.4 dep-inversion: layer-input types (`OptionOverride`,
// `OptionOverrideSet`, `OverridePriority`) live here now.
// Previously hosted in `lattice-mode` to break a cycle through
// `lattice-core -> lattice-mode -> lattice-config`; the cycle
// was retired by removing `Document::modes` from lattice-core.
// With the cycle gone, the override types belong in lattice-
// config alongside the resolver and the typed-options layer they
// override against.
pub mod overrides;
mod parse;
#[cfg(test)]
mod proc_macro_tests;
mod registry;
mod resolved;
mod resolver;

// Re-export `linkme` so the proc macros can reference
// `::lattice_config::linkme::distributed_slice` reliably when
// expanded outside this crate.
#[doc(hidden)]
pub use linkme;

// Re-export the proc macros from `lattice-config-macros`.
// Users write `lattice_config::options! { ... }` /
// `groups! { ... }` / `overrides! { ... }`; the proc-macro
// crate is a private implementation detail.
pub use lattice_config_macros::{groups, options, overrides};

pub use completion::OptionsGenerator;
// M.2.0c: re-export the macro-generated option types at the
// crate root for ergonomic type-keyed access.
// Callers write `config.get_typed::<lattice_config::Tabstop>()`
// instead of the longer `lattice_config::core_options::Tabstop`.
pub use core_options::COMPLETION_SOURCE_SNIPPET_DEFAULT_PRIORITY;
pub use core_options::{
    CompletionAutoInsertSingle, CompletionExtraCommitChars, CompletionGhostText,
    CompletionSourceBufferWordsPriority, CompletionSourceLspPriority, CompletionSourcePathPriority,
    CompletionSourceSnippetPriority, CompletionSourceTreeSitterPriority, CursorLine,
    DiagnosticsInlineOption, DiagnosticsMinSeverityOption, FoldEnable,
    FoldMethodOption, HelpAproposDisplay, HelpDescribeDisplay, HelpListDisplay, HelpTopicDisplay,
    HoverDisplay, IgnoreCase, LspLogDisplay, LspStatusDisplay, MessagesDisplay, MessagesFilter,
    ModelineCenter, ModelineLeft, ModelinePadding, ModelineRight, ModelineSeparator,
    NoFile, Number, PickerResultDisplay, ReadOnly, RelativeNumber, Scrollbind, Scrolloff,
    Sidescroll, Sidescrolloff, SignColumnOption, SignatureDisplay,
    TablineShowOption, Tabstop, TerminalEscExits, TerminalScrollbackLines, Whitespace,
    WhitespaceEol, WhitespaceLeading, WhitespaceSpace, WhitespaceTab, WhitespaceTrailing, Wrap,
};
pub use erased::ErasedOption;
pub use group::{
    Appearance, Completion, Diagnostics, Display, Editing, Editor, Filetree, GROUP_DECLS, Help, Lsp,
    Messages, Modeline, Oil, OptionGroup, OptionGroupMetadata, Picker, Search, Snippet, Tabline,
    Terminal, ends_with_mode_suffix,
};
pub use loader::{
    LoadMessage, LoadMessageLevel, LoadOutcome, config_home, default_user_config_path,
    load_default_paths, load_file, lookup_dotted_path, project_config_path,
};
// ML.5: the modeline zone-layout value type (`ui.modeline.{left,center,
// right}`). The first list-valued option; see `modeline_zone`.
pub use modeline_zone::ModelineZone;
// L4a: inline-diagnostics option value types (`ui.diagnostics.*`).
pub use diagnostics_options::{DiagnosticsInline, DiagnosticsSeverity};
// PU.1b: the `signcolumn` option value type — gates the gutter sign
// columns (diagnostics severity + diff sign) so help / synthetic
// buffers render gutterless without the renderer knowing it's help.
pub use signcolumn::SignColumn;
// M.2.0c: `Option<T>`, `OptionBuilder<T>`, `OptionHandle<T>`
// remain `pub` from the `option` module so the macros' generated
// `build_spec()` methods can name them, but they are no longer
// re-exported at the crate root. The intended public surface is
// the macro path -- callers declare options via `options! { ... }`
// and read via `config.get_typed::<X>()`. Direct construction of
// `Option<T>` survives for the future plugin-adapter path.
pub use option_decl::{HasGroup, OPTION_DECLS, OptionDecl, OptionDeclMetadata};
pub use option_type::OptionType;
// Layer-input types live in this crate now (post M.4 dep
// inversion). Modes pull them in via lattice-mode's re-export.
pub use origin::OptionOrigin;
pub use overrides::{OptionOverride, OptionOverrideSet, OverridePriority};
pub use parse::{ParsedSet, parse_set};
pub use registry::{ConfigError, ConfigRegistry, EventPublisher};
pub use resolved::ResolvedOptions;
pub use resolver::Resolver;
