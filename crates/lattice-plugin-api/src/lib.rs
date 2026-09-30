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
    /// The types this interface DEFINES, in WIT source order (authors order a
    /// seam's types so each is read after what it depends on).
    pub types: Vec<ApiType>,
    /// The types this interface pulls in from another with `use`, in source
    /// order. Listed apart from [`types`](Self::types) so a reference links to
    /// the definition instead of repeating it.
    pub uses: Vec<ApiUse>,
}

/// One function within an interface.
///
/// Resource methods are functions too: WIT names them `[method]<resource>.<name>`
/// (`[static]…`, `[constructor]<resource>` likewise) and [`kind`](Self::kind)
/// says which resource they belong to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiFunction {
    /// The WIT function name, e.g. `walk` or `[method]document.line`.
    pub name: String,
    /// The function's `///` doc comment, if any.
    pub doc: Option<String>,
    /// Freestanding, or which resource it is a method / static / constructor of.
    pub kind: ApiFunctionKind,
    /// Declared `async` in the WIT.
    pub is_async: bool,
    /// Parameters in order. A method's first parameter is `self`.
    pub params: Vec<ApiParam>,
    /// The result type in WIT syntax, or `None` for a function returning nothing.
    pub result: Option<String>,
}

impl ApiFunction {
    /// The name a reader calls it by: `walk`, `document.line`,
    /// `document.new` for a constructor. Strips WIT's `[method]` /
    /// `[static]` / `[constructor]` mangling.
    pub fn display_name(&self) -> String {
        match &self.kind {
            ApiFunctionKind::Freestanding => self.name.clone(),
            ApiFunctionKind::Constructor(r) => format!("{r}.new"),
            ApiFunctionKind::Method(_) | ApiFunctionKind::Static(_) => self
                .name
                .split_once(']')
                .map(|(_, rest)| rest.to_string())
                .unwrap_or_else(|| self.name.clone()),
        }
    }

    /// The function's signature in WIT syntax, as it would be declared inside
    /// its interface (or its resource block): `walk: func(opts: walk-options)
    /// -> result<list<string>, string>`. A method's implicit `self` is omitted,
    /// as WIT source omits it.
    pub fn signature(&self) -> String {
        let params = |skip: usize| {
            self.params
                .iter()
                .skip(skip)
                .map(|p| format!("{}: {}", p.name, p.ty))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let result = self
            .result
            .as_ref()
            .map(|r| format!(" -> {r}"))
            .unwrap_or_default();
        let asyncness = if self.is_async { "async " } else { "" };
        let short = self.display_name();
        let short = short.rsplit('.').next().unwrap_or(&self.name);
        match &self.kind {
            ApiFunctionKind::Freestanding => {
                format!("{}: {asyncness}func({}){result}", self.name, params(0))
            }
            ApiFunctionKind::Method(_) => {
                format!("{short}: {asyncness}func({}){result}", params(1))
            }
            ApiFunctionKind::Static(_) => {
                format!("{short}: static {asyncness}func({}){result}", params(0))
            }
            // A constructor's declared result is the resource itself; WIT
            // source does not spell it.
            ApiFunctionKind::Constructor(_) => format!("constructor({})", params(0)),
        }
    }
}

/// Whether a function stands alone or belongs to a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiFunctionKind {
    /// A plain interface function.
    Freestanding,
    /// A method on the named resource (first parameter is `self`).
    Method(String),
    /// A static function on the named resource.
    Static(String),
    /// The named resource's constructor.
    Constructor(String),
}

/// One function parameter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiParam {
    /// Parameter name as declared.
    pub name: String,
    /// Parameter type in WIT syntax.
    pub ty: String,
}

/// One type an interface defines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiType {
    /// Kebab-case type name, e.g. `raw-candidate`.
    pub name: String,
    /// The type's `///` doc comment, if any.
    pub doc: Option<String>,
    /// What kind of type it is, with its fields or cases.
    pub kind: ApiTypeKind,
}

/// The shape of a type definition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApiTypeKind {
    /// A `record`: every field, in order, each with a type.
    Record(Vec<ApiMember>),
    /// A `variant`: every case, in order; a case may carry a payload type.
    Variant(Vec<ApiMember>),
    /// An `enum`: every case, in order (no payloads).
    Enum(Vec<ApiMember>),
    /// A `flags`: every flag, in order.
    Flags(Vec<ApiMember>),
    /// A `resource`: a host- or guest-owned handle. Its methods are the
    /// interface's functions whose [`ApiFunction::kind`] names it.
    Resource,
    /// `type name = <expr>`: the aliased type in WIT syntax.
    Alias(String),
}

impl ApiTypeKind {
    /// The WIT keyword for this kind: `record`, `variant`, `enum`, `flags`,
    /// `resource`, or `type` for an alias.
    pub fn keyword(&self) -> &'static str {
        match self {
            ApiTypeKind::Record(_) => "record",
            ApiTypeKind::Variant(_) => "variant",
            ApiTypeKind::Enum(_) => "enum",
            ApiTypeKind::Flags(_) => "flags",
            ApiTypeKind::Resource => "resource",
            ApiTypeKind::Alias(_) => "type",
        }
    }

    /// The fields / cases / flags, empty for a resource or alias.
    pub fn members(&self) -> &[ApiMember] {
        match self {
            ApiTypeKind::Record(m)
            | ApiTypeKind::Variant(m)
            | ApiTypeKind::Enum(m)
            | ApiTypeKind::Flags(m) => m,
            ApiTypeKind::Resource | ApiTypeKind::Alias(_) => &[],
        }
    }
}

/// A record field, variant case, enum case or flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiMember {
    /// Kebab-case member name.
    pub name: String,
    /// The member's type in WIT syntax: always present for a record field,
    /// the payload for a variant case that has one, `None` otherwise.
    pub ty: Option<String>,
    /// The member's `///` doc comment, if any.
    pub doc: Option<String>,
}

/// A type an interface imports from another with `use`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiUse {
    /// The name it is known by in this interface (after any `as` rename).
    pub name: String,
    /// The interface that defines it.
    pub from: String,
    /// Its name in the defining interface.
    pub original: String,
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
    /// Freestanding functions the world exports — the guest's entry points
    /// (`register-grammar`, `register-picker-sources`, …), in WIT source
    /// order. Not part of any interface, so they appear nowhere else in the
    /// catalog.
    pub export_functions: Vec<ApiFunction>,
    /// Freestanding functions the world imports from the host, in source
    /// order.
    pub import_functions: Vec<ApiFunction>,
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
/// seams are pure data/dispatch (`None`). Three reach the OS: `host-services`
/// (`Fs` — its `walk`), `project` (`Fs` — resolution walks on a cache miss),
/// and `plugin-manager` (`Proc` — the host builds what the guest declared; see
/// the row's own comment for why one variant cannot describe it).
pub const CAPABILITY_ANNOTATIONS: &[(&str, Capability)] = &[
    // OM.A1. `None` and not `Fs`, which is the interesting part: the host
    // walks the project and reads every file, and hands the guest `text`. The
    // guest touches no filesystem — no preopens, no `walk` — so the capability
    // this seam requires *of a plugin* is nothing. Keeping the read host-side
    // is what makes that true, and annotating it `Fs` would misreport a
    // deliberate design property as a permission.
    ("scanned-excerpt-source", Capability::None),
    // The three REGISTRY-shaped seams, `None` for `scanned-excerpt-source`'s
    // reason immediately above and not by default: in each the host owns the
    // mechanism — the walk, the view, the picker — and the guest declares what
    // to make of it or receives what the host already read. The capability
    // belongs to the host's I/O, not to the seam that describes it. A seam
    // where the GUEST reaches the filesystem would be annotated `Fs` here even
    // though its WIT looks the same, which is why this table is hand-written.
    ("multibuffer-view-registry", Capability::None),
    ("multibuffer-view-source", Capability::None),
    ("picker-registry", Capability::None),
    // MV.3: the host owns the view, the walk and every file read; a guest
    // declares a view spec and receives excerpts. `None` for
    // `scanned-excerpt-source`'s reason, immediately above — the capability
    // belongs to the host's walk, not to the seam that describes what to make
    // of it.
    ("multibuffer-view-registry", Capability::None),
    ("multibuffer-view-source", Capability::None),
    ("buffer", Capability::None),
    ("command", Capability::None),
    ("completion-source", Capability::None),
    ("config", Capability::None),
    ("context", Capability::None),
    ("dashboard", Capability::None),
    ("decorations", Capability::None),
    ("error-parser", Capability::None),
    ("events", Capability::None),
    ("grammar", Capability::None),
    ("grammar-callbacks", Capability::None),
    ("help", Capability::None),
    ("host-services", Capability::Fs),
    ("keymap", Capability::None),
    // IM.6. Same shape as `scanned-excerpt-source` above: the guest names a file and
    // never sends pixels, and the host resolves + reads it. `media.wit` calls
    // that out as deliberate — "the `fs:read` capability decision stays with
    // the HOST, which is what stops a plugin putting arbitrary bytes on screen
    // regardless of its grant". So the seam demands nothing of the plugin.
    ("media", Capability::None),
    // LG.3c. The guest hands over grammar BYTES and query source; the host
    // compiles and runs them. No OS reach: the grammar is wasm the host
    // executes inside tree-sitter's own sandboxed store, not native code and
    // not a file the guest names. Fetching a grammar from git is the plugin
    // MANAGER's job (`Proc`/net, that row), not this seam's.
    ("language", Capability::None),
    ("logging", Capability::None),
    ("modes", Capability::None),
    ("picker-source", Capability::None),
    // The `require` seam. The GUEST does nothing but declare a list; the HOST
    // then clones or downloads the source (net), stages it into the user
    // plugin root (fs), and runs cargo-component over it (proc). `Capability`
    // is single-valued, so this row cannot say "all three" — it names the
    // widest blast radius and the doc above says why. If a second seam ever
    // needs a union, that is the point to make `Capability` a set rather than
    // keep picking a representative.
    ("plugin-manager", Capability::Proc),
    // Resolution walks the filesystem on a cache miss. This is not a
    // formality: `error-parser-plugin` deliberately does NOT import `project`
    // for exactly this reason (see `wit/error-parser.wit`), because that world
    // shares the sync linker with `grammar` and a directory walk one guest
    // call from a keystroke is a paramount-#1 violation. Annotating this
    // `None` would contradict the decision that WIT already records.
    ("project", Capability::Fs),
    // SG.3a. `None`: declaring a sign is pure data in one direction — the
    // guest names a glyph, a fallback glyph, a theme element and a priority,
    // and the host writes its OWN registry. No filesystem, no network, and the
    // name is namespaced host-side so a guest cannot reach another plugin's
    // signs or a native producer's.
    ("signs", Capability::None),
    ("theme", Capability::None),
    // TR.2b. `None`: a keyed menu is pure data in both directions — the host
    // projects where the menu was opened from, the guest answers rows naming
    // commands. The guest touches no filesystem, no network, and cannot forge
    // a `CommandId` (names are resolved host-side).
    ("transient-source", Capability::None),
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

pub mod examples;
pub mod json;
pub mod render;

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

    /// The interface that DEFINES the type `name`, with the definition.
    /// Interfaces that merely `use` it are not answers.
    pub fn type_def(&self, name: &str) -> Option<(&ApiInterface, &ApiType)> {
        self.interfaces
            .iter()
            .find_map(|i| i.types.iter().find(|t| t.name == name).map(|t| (i, t)))
    }
}
