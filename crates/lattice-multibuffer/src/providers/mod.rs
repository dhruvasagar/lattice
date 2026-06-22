//! M.6 (2026-06-01) onwards: in-tree multibuffer providers.
//!
//! Each provider is one user-facing surface (project-search,
//! lsp-references, project-diff, etc.) built on top of the
//! multibuffer machinery. Provider code lives in this crate per
//! the 2026-06-01 architecture decision (`multibuffer-views.md`
//! §3.7), behind individual cargo features:
//!
//! - `search` — `providers::search`, the worked SearchProvider
//!   example. Walks the filesystem via `ignore::Walk`,
//!   matches literal queries (regex landing in a follow-up),
//!   streams batches via typed `ProjectSearch*` events.
//!
//! Each provider declares: a service trait + handle, typed
//! events for its scan / fetch lifecycle, a minor mode contributing
//! provider-specific keymap + lifecycle subscriptions, a public
//! trigger function, and a boot helper that wires the service
//! into `ServiceRegistry` + registers the mode against
//! `ModeRegistry`.

#[cfg(feature = "search")]
pub mod search;

/// N.1.1 (2026-06-10): narrow mode — a focused, editable one-excerpt
/// multibuffer view of a region. Unlike `search`, narrow is a
/// first-class built-in (no cargo feature gate): no async scan, no
/// service, no per-view state beyond the excerpt.
pub mod narrow;
