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
//! - **Event publication.** The follow-up task wires the registry
//!   to optionally publish `Event::OptionChanged` on every set
//!   (per the §5.10 event bus). Renderers will subscribe to that
//!   event for cascade refreshes instead of polling.

pub mod completion;
mod core_options;
mod domain;
mod erased;
mod option;
mod option_type;
mod parse;
mod registry;

pub use completion::OptionsGenerator;
pub use core_options::{CoreOptions, register_core_options};
pub use erased::ErasedOption;
pub use option::{Option, OptionBuilder, OptionHandle};
pub use option_type::OptionType;
pub use parse::{ParsedSet, parse_set};
pub use registry::{ConfigError, ConfigRegistry};
