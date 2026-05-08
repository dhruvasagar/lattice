//! Typed configuration registry (DESIGN.md §5.12).
//!
//! Renderer-agnostic option machinery: the [`OptionType`] trait,
//! the typed [`Option<T>`] spec, the type-erased [`ErasedOption`]
//! trait the registry stores, and [`ConfigRegistry`] itself.
//!
//! ## Design (γ — value-on-spec storage)
//!
//! Each [`Option<T>`] owns its current value behind an
//! [`arc_swap::ArcSwap<T>`]. Reads through a typed
//! [`OptionHandle<T>`] are wait-free pointer loads. Writes go
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
//! plus this crate's [`Color`] when the foreign-type problem is
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
mod domain;
mod erased;
pub mod group;
pub mod loader;
// `option` is `pub` so the proc macros' generated code can name
// `::lattice_config::option::Option<T>` for runtime spec
// construction. Direct construction of `Option<T>` is the
// macro-internal path (the macros' `build_spec()` calls
// `Option::<T>::builder(...)`); consumer-level code uses the
// macro and `config.get_typed::<X>()` instead.
pub mod option;
mod option_decl;
mod option_type;
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
pub use core_options::{
    CompletionAutoInsertSingle, CompletionExtraCommitChars, CompletionGhostText,
    CompletionSourceBufferWordsPriority, CompletionSourceLspPriority,
    CompletionSourcePathPriority, CompletionSourceSnippetPriority,
    CompletionSourceTreeSitterPriority, FoldEnable, FoldMethodOption, IgnoreCase, Number,
    RelativeNumber, Scrolloff, Tabstop, Wrap,
};
pub use erased::ErasedOption;
pub use group::{
    Appearance, Completion, Display, Editing, Editor, Filetree, GROUP_DECLS, Help, Lsp,
    OptionGroup, OptionGroupMetadata, Oil, Picker, ends_with_mode_suffix,
};
pub use loader::{
    LoadMessage, LoadMessageLevel, LoadOutcome, default_user_config_path, load_default_paths,
    load_file, lookup_dotted_path, project_config_path,
};
// M.2.0c: `Option<T>`, `OptionBuilder<T>`, `OptionHandle<T>`
// remain `pub` from the `option` module so the macros' generated
// `build_spec()` methods can name them, but they are no longer
// re-exported at the crate root. The intended public surface is
// the macro path -- callers declare options via `options! { ... }`
// and read via `config.get_typed::<X>()`. Direct construction of
// `Option<T>` survives for the future plugin-adapter path.
pub use option_decl::{HasGroup, OPTION_DECLS, OptionDecl, OptionDeclMetadata};
pub use option_type::OptionType;
// Re-export the layer-input types from lattice-mode at the
// lattice-config crate root so consumers (modes, plugins,
// future buffer-local-set machinery) get one canonical import
// surface for the option system. Definitions live in
// lattice-mode (per the dependency-cycle rationale documented
// in lattice-mode::overrides), but ergonomically the user
// imports them from lattice-config alongside the registry.
pub use lattice_mode::{OptionOverride, OptionOverrideSet, OverridePriority};
pub use parse::{ParsedSet, parse_set};
pub use registry::{ConfigError, ConfigRegistry, EventPublisher};
pub use resolved::ResolvedOptions;
pub use resolver::Resolver;
