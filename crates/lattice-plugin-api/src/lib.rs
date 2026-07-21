//! Plugin-API introspection catalog (PI.1).
//!
//! The `wit/` package at the workspace root IS the canonical plugin API
//! (plugin-host.md §5). This crate exposes a [`PluginApiCatalog`] *derived from
//! that WIT at build time* (`build.rs` → `wit-parser` → `$OUT_DIR/catalog.rs`),
//! so the catalog can never drift from the interface it documents. It answers
//! the "what CAN a plugin do" facet of the introspection layer (design §5.11);
//! `:describe-plugin-api` / `:list-plugin-apis` / `:apropos` (PI.2) render it,
//! and plugin authors export it (JSON/markdown).
//!
//! This crate is deliberately **wasmtime-free** — its only build input is the
//! WIT text and its only runtime dep is `std`. `lattice-host` can therefore dep
//! it for the introspection ex-commands WITHOUT pulling the WASM runtime into
//! the host, keeping the no-per-frame-WASM invariant (plugin-host.md PH7.5).
//!
//! Two things the catalog carries that the parser can't infer:
//!   - **direction** — world-derived (does a guest *export* the interface, i.e.
//!     implement it, or *import* it, i.e. call into the host); a descriptive
//!     hint, since `use`-for-types also registers an import edge.
//!   - **capability** — a host-authored annotation the WIT can't express (which
//!     OS capability a seam requires); see [`CAPABILITY_ANNOTATIONS`]. Every
//!     parsed interface MUST have an entry (enforced by a test), so a new WIT
//!     interface forces a deliberate capability decision before it ships.

use std::sync::OnceLock;

/// The whole plugin-API surface, derived from `wit/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginApiCatalog {
    /// Every named interface in the package, sorted by name.
    pub interfaces: Vec<ApiInterface>,
    /// Every plugin world (the test-only `trampoline-fixture` excluded), sorted
    /// by name.
    pub worlds: Vec<ApiWorld>,
}

/// One WIT interface — a namespace of functions a plugin implements or calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiInterface {
    /// Kebab-case interface name, e.g. `host-services`.
    pub name: String,
    /// The interface's `///` doc comment, if any.
    pub doc: Option<String>,
    /// World-derived direction relative to a guest plugin.
    pub direction: Direction,
    /// Host-authored capability requirement (the WIT can't carry it).
    pub capability: Capability,
    /// The interface's functions, sorted by name.
    pub functions: Vec<ApiFunction>,
}

/// One function within an interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFunction {
    /// Kebab-case function name, e.g. `walk`.
    pub name: String,
    /// The function's `///` doc comment, if any.
    pub doc: Option<String>,
}

/// One WIT world — a bundle of imported/exported interfaces a component targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiWorld {
    /// Kebab-case world name, e.g. `picker-source-plugin`.
    pub name: String,
    /// The world's `///` doc comment, if any.
    pub doc: Option<String>,
    /// Interface names the world imports (guest → host), sorted.
    pub imports: Vec<String>,
    /// Interface names the world exports (guest implements), sorted.
    pub exports: Vec<String>,
}

/// World-derived direction of an interface relative to a guest plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Guest *implements* it (a world exports it): `grammar`, `picker-source`, …
    GuestExport,
    /// Guest *calls into the host* through it (a world imports it): `host-services`.
    GuestImport,
    /// Both an export and an import edge exist across worlds.
    Both,
    /// Neither — a shared type bag (`types`) or a still-stub interface,
    /// referenced only via `use` for its types.
    TypesOnly,
}

/// A host-authored capability annotation the WIT can't itself carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Filesystem access (e.g. `host-services::walk`).
    Fs,
    /// Network access.
    Net,
    /// Subprocess spawn.
    Proc,
    /// No OS capability — a pure data / dispatch seam.
    None,
}

/// The host-authored capability annotation, one row per WIT interface.
///
/// A test asserts this covers EVERY parsed interface, so adding a WIT interface
/// without a deliberate capability decision fails the build's test gate. Most
/// seams are pure data/dispatch (`None`); only `host-services` reaches the OS
/// today (`Fs` — its `walk`; `net`/`proc` seams will refine this row when they
/// land, design.md §15).
pub const CAPABILITY_ANNOTATIONS: &[(&str, Capability)] = &[
    ("buffer", Capability::None),
    ("command", Capability::None),
    ("completion-source", Capability::None),
    ("config", Capability::None),
    ("decorations", Capability::None),
    ("events", Capability::None),
    ("grammar", Capability::None),
    ("grammar-callbacks", Capability::None),
    ("host-services", Capability::Fs),
    ("keymap", Capability::None),
    ("logging", Capability::None),
    ("modes", Capability::None),
    ("picker-source", Capability::None),
    ("tree-sitter", Capability::None),
    ("types", Capability::None),
    ("ui", Capability::None),
];

/// The capability annotation for an interface, or `None` if unannotated (which
/// the coverage test forbids for any parsed interface).
pub fn capability_for(name: &str) -> Option<Capability> {
    CAPABILITY_ANNOTATIONS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, c)| *c)
}

// The parsed catalog data (`generated_interfaces()` / `generated_worlds()`),
// emitted by build.rs from wit/. Private free functions in this module scope.
include!(concat!(env!("OUT_DIR"), "/catalog.rs"));

/// The plugin-API catalog, derived from `wit/` at build time and merged with
/// the host-authored capability annotation. Computed once, then cached.
pub fn catalog() -> &'static PluginApiCatalog {
    static CATALOG: OnceLock<PluginApiCatalog> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut interfaces = generated_interfaces();
        for iface in &mut interfaces {
            iface.capability = capability_for(&iface.name).unwrap_or(Capability::None);
        }
        PluginApiCatalog {
            interfaces,
            worlds: generated_worlds(),
        }
    })
}

impl PluginApiCatalog {
    /// The interface with this exact name, if present.
    pub fn interface(&self, name: &str) -> Option<&ApiInterface> {
        self.interfaces.iter().find(|i| i.name == name)
    }

    /// The world with this exact name, if present.
    pub fn world(&self, name: &str) -> Option<&ApiWorld> {
        self.worlds.iter().find(|w| w.name == name)
    }
}
