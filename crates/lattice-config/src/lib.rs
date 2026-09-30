//! The typed options system: every editor, mode, renderer and plugin
//! setting is a registered, typed value in one [`ConfigRegistry`], read
//! on the hot path by type and written at the boundaries (`:set`,
//! `lattice.toml`, `:setlocal`, plugins) by name (DESIGN.md §5.12).
//!
//! Renderer-agnostic option machinery: the [`OptionType`] trait,
//! the typed [`option::Option<T>`] spec, the type-erased [`ErasedOption`]
//! trait the registry stores, and [`ConfigRegistry`] itself.
//!
//! ## What it owns
//!
//! - **Declaration.** The [`options!`] / [`groups!`] macros (from
//!   `lattice-config-macros`) turn a declaration into an [`OptionDecl`]
//!   marker type plus an [`OptionDeclMetadata`] entry in the
//!   [`OPTION_DECLS`] link-time slice; [`ConfigRegistry::init_from_linkme`]
//!   registers every one linked into the binary. The built-in set lives
//!   in [`core_options`] and is re-exported here (`Tabstop`, `Wrap`, ...);
//!   groups ([`OptionGroup`]) organise them for `:customize`.
//! - **Values.** [`OptionType`] (parse / format / enumerate / schema) with
//!   impls for `bool`, `i64`, `String`, the lattice-core domain enums, and
//!   the display-policy value types defined here ([`SignColumn`],
//!   [`ModelineZone`], [`DiagnosticsInline`], [`Decorations`], ...).
//!   [`ConfigSchema`] / [`ConfigValue`] describe composite (list / record)
//!   values as data — see [`schema`].
//! - **Access.** Type-keyed reads/writes ([`ConfigRegistry::get_typed`] /
//!   [`ConfigRegistry::set_typed`]), handle reads for runtime-registered
//!   options ([`option::OptionHandle`]), and the by-name `:set` path
//!   ([`parse_set`] → [`ConfigRegistry::parse_and_set_command`]).
//! - **Layering.** Per-buffer resolution: modes and `:setlocal` contribute
//!   [`OptionOverrideSet`]s, the [`Resolver`] merges them by layer and
//!   [`OverridePriority`] into a [`ResolvedOptions`] cache, each winner
//!   tagged with its [`OptionOrigin`].
//! - **Files.** The TOML [`loader`] (`lattice.toml`, `.lattice/config.toml`)
//!   and the config-home paths ([`config_home`], [`cache_home`]).
//! - **Completion.** [`OptionsGenerator`], the `:set <Tab>` candidate source.
//!
//! ## What it must not depend on
//!
//! Its dependencies are the foundation only (`lattice-protocol`,
//! `lattice-core`, `lattice-completion`). Almost every crate reads options,
//! so anything this crate imported would sit beneath the whole editor:
//! no host / `App`, no renderer, no mode registry, no event bus
//! (`lattice-runtime` — events leave through an injected
//! [`EventPublisher`] closure instead), no plugin host (so
//! [`PluginTraceLevel`] duplicates the host's trace-level labels rather
//! than importing the type). The layer-input override types moved *into*
//! this crate for the same reason (see [`mod@overrides`]).
//!
//! ## Example
//!
//! ```
//! use lattice_config::{ConfigRegistry, Tabstop};
//!
//! let config = ConfigRegistry::new();
//! config.init_from_linkme(); // register every `options!` declaration
//!
//! // Hot path: read by type.
//! assert_eq!(*config.get_typed::<Tabstop>().unwrap(), 4);
//!
//! // Boundary: write by name, exactly as `:set ts=2` does.
//! assert_eq!(config.parse_and_set_command("ts=2").unwrap(), "tabstop=2");
//! assert_eq!(*config.get_typed::<Tabstop>().unwrap(), 2);
//!
//! // Validation runs on every write; a rejected write changes nothing.
//! assert!(config.parse_and_set_command("tabstop=99").is_err());
//! assert_eq!(*config.get_typed::<Tabstop>().unwrap(), 2);
//! ```
//!
//! Design: `docs/dev/architecture/typed-configuration.md` (schemas, composite
//! values), `docs/dev/architecture/config-and-init.md` (when configuration
//! arrives), `docs/dev/architecture/buffer-local-options.md` (`:setlocal`,
//! origins), `docs/dev/architecture/mode-architecture.md` §6 (declarations,
//! layering, groups).
//!
//! ## Design (γ — value-on-spec storage)
//!
//! Each [`option::Option<T>`] owns its current value behind an
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
//! The trait + the three primitive impls (`bool`, `i64`, `String`)
//! live here. So do the impls for the `lattice-core` domain enums
//! (`FoldMethod`, `IndentMethod`, ...): the orphan rule allows a local
//! trait on a foreign type, and `lattice-core` cannot depend on this
//! crate. A crate *above* this one that defines its own value type
//! implements [`OptionType`] itself, importing the trait from
//! `lattice-config`.
//!
//! ## What's NOT here
//!
//! - **`App` reference.** Setters don't take an `&mut App`. The
//!   value lives in the spec; consumers read it through their
//!   typed handle. Renderers run side-effect cascades
//!   (`relativenumber` ⇒ `number`, `foldmethod` ⇒ recompute folds,
//!   `ui.*` ⇒ refresh derived theme styles) in their own
//!   post-set hook, polling the parsed `:set` form.
//! - **Other crates' options.** Options owned elsewhere are declared
//!   with the same [`options!`] macro in the owning crate and join the
//!   registry through [`OPTION_DECLS`] at boot — e.g. the `ui.separator`
//!   / `ui.statusline_*_fg` family in `lattice_host::ui::theme_options`.
//!   Runtime-only options (plugins) use [`ConfigRegistry::register`].
//!
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

#![warn(missing_docs)]

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
mod decorations;
mod diagnostics_options;
mod domain;
mod erased;
mod expand_height;
pub mod group;
pub mod loader;
mod modeline_zone;
mod pane_options;
mod plugin_options;
// PR.2: the `project.root-markers` list option.
mod root_markers;
mod signcolumn;
mod window_options;
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
/// TC.1: `ConfigSchema` / `ConfigValue` — what an option's value is shaped
/// like, as data, plus schema-checked validation that reports a path.
pub mod schema;
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
    AutoWrapOption, ClipboardEnabled, CommandLineExpandHeight, CompletionAutoInsertSingle,
    CompletionExtraCommitChars, CompletionGhostText, CompletionSourceBufferWordsPriority,
    CompletionSourceLspPriority, CompletionSourcePathPriority, CompletionSourceSnippetPriority,
    CompletionSourceTreeSitterPriority, CursorLine, DiagnosticsInlineOption,
    DiagnosticsMinSeverityOption, ElectricIndent, ExpandTab, FoldEnable, FoldMethodOption,
    FormatIndentChain, FormatOnSave, FormatPrg, FormatReflowChain, FormatReformatChain,
    HelpAproposDisplay, HelpDescribeDisplay, HelpListDisplay, HelpTopicDisplay, HoverDisplay,
    IgnoreCase, IndentMethodOption, LspLogDisplay, LspStatusDisplay, MessagesDisplay,
    MessagesFilter, ModelineCenter, ModelineLeft, ModelinePadding, ModelineRight,
    ModelineSeparator, MouseEnabled, NoFile, Number, PickerResultDisplay, ProjectRootMarkers,
    ReadOnly, RelativeNumber, Scroll, Scrollbind, Scrolloff, Shiftwidth, Sidescroll, Sidescrolloff,
    SignColumnOption, SignatureDisplay, StartOfLine, TablineShowOption, Tabstop, TerminalEscExits,
    TerminalScrollbackLines, TextWidth, TransientMaxRows, Whitespace, WhitespaceEol,
    WhitespaceLeading, WhitespaceSpace, WhitespaceTab, WhitespaceTrailing, Wrap,
};
pub use erased::ErasedOption;
pub use group::{
    Ai, Appearance, Completion, Diagnostics, Display, Editing, Editor, Filetree, GROUP_DECLS, Help,
    Lsp, Magit, Messages, Modeline, Mouse, Notifications, Oil, OptionGroup, OptionGroupMetadata,
    Pane, Picker, Plugin, Project, Search, Snippet, Tabline, Terminal, Window,
    ends_with_mode_suffix,
};
pub use loader::{
    LoadMessage, LoadMessageLevel, LoadOutcome, cache_home, cache_home_from, config_home,
    default_user_config_path, load_default_paths, load_file, lookup_dotted_path, migrate_path,
    project_config_path,
};
// ML.5: the modeline zone-layout value type (`ui.modeline.{left,center,
// right}`). The first list-valued option; see `modeline_zone`.
pub use modeline_zone::ModelineZone;
// PR.2: the `project.root-markers` value type. Note `Project` above is
// the option GROUP marker (`:customize project`), not `lattice_core::Project`
// — the resolved root. Different crates, different jobs.
pub use root_markers::RootMarkers;
// L4a: inline-diagnostics option value types (`ui.diagnostics.*`).
pub use diagnostics_options::{DiagnosticsInline, DiagnosticsSeverity};
// PO.4.3: the `plugin.trace-level` option value + decl type — the global default
// plugin boundary-trace verbosity (the loader observes changes → the tracer gate).
pub use plugin_options::{PluginTraceLevel, PluginTraceLevelOption};
// PU.1b: the `signcolumn` option value type — gates the gutter sign
// columns (diagnostics severity + diff sign) so help / synthetic
// buffers render gutterless without the renderer knowing it's help.
pub use signcolumn::SignColumn;
// MB.2e: the `command-line.expand-height` option value type — how tall
// the expanded `:` mini-buffer band grows (`half` / `full` / fixed rows).
pub use expand_height::ExpandHeight;
// W.1: the `decorations` option value type — controls OS window chrome
// (titlebar + controls) on the GPUI peer (full vs. borderless).
pub use decorations::Decorations;
// W.2: the `ui.window.*` decl types — GPUI peer window chrome +
// maximize-on-launch. Value type (`Decorations`) re-exported above.
pub use pane_options::{PaneBufferHistorySize, PaneZoomIndicator};
pub use window_options::{StartMaximized, WindowDecorationsOption};
// M.2.0c: `Option<T>`, `OptionBuilder<T>`, `OptionHandle<T>`
// remain `pub` from the `option` module so the macros' generated
// `build_spec()` methods can name them, but they are no longer
// re-exported at the crate root. The intended public surface is
// the macro path -- callers declare options via `options! { ... }`
// and read via `config.get_typed::<X>()`. Direct construction of
// `Option<T>` survives for the future plugin-adapter path.
pub use option_decl::{HasGroup, OPTION_DECLS, OptionDecl, OptionDeclMetadata};
pub use option_type::OptionType;
// TC.1: an option's shape as data, and values in that shape
// (`typed-configuration.md`).
pub use schema::{ConfigSchema, ConfigValue, ScalarKind, SchemaError, SchemaField};
// Layer-input types live in this crate now (post M.4 dep
// inversion). Modes pull them in via lattice-mode's re-export.
pub use origin::OptionOrigin;
pub use overrides::{OptionOverride, OptionOverrideSet, OverridePriority};
pub use parse::{ParsedSet, parse_set};
pub use registry::{ConfigError, ConfigRegistry, EventPublisher, plugin_option_groups};
pub use resolved::ResolvedOptions;
pub use resolver::Resolver;
