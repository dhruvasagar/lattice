//! Picker source registry (metadata layer).
//!
//! Each picker source — `files`, `recent`, `lines`, `marks`,
//! `lsp-references`, ... — registers a [`PickerSourceSpec`]
//! into a [`PickerRegistry`] at boot. The registry powers
//! three things:
//!
//! 1. **`:picker <Tab>` completion.** The cmdline source-id
//!    completion mode iterates the registry and surfaces every
//!    registered source as a candidate.
//! 2. **Source-arg completion.** Once the source id is resolved,
//!    arg-2+ completion consults the source's
//!    [`ArgSpec::completion`] hooks — same `gen:*` completion
//!    sources every other ex-command uses.
//! 3. **`:describe-picker` introspection.** Walks the registry
//!    to render `:describe-picker` (and `:describe-picker <id>`
//!    for per-source detail).
//!
//! The registry only holds **metadata** at this stage. The
//! `PickerSourceGenerator` trait (slice 4 in
//! `docs/dev/architecture/picker.md`) elevates the registry to
//! hold generator trait objects so source dispatch is registry-
//! driven end-to-end. Today the App still owns the
//! `source_id → method` dispatch table; the registry just
//! supplies the names + arg schemas the grammar needs.
//!
//! ## WIT mirror (Phase 7)
//!
//! When the plugin host lands, WIT-imported sources register
//! their spec record into the same `PickerRegistry`. Plugin
//! sources are indistinguishable from first-party at the
//! registry level — both appear under `:picker <Tab>` and
//! flow through the same dispatch.
//!
//! The registry interface is therefore deliberately small:
//! `register`, `get`, `iter`. Nothing host-specific leaks in.

use std::borrow::Cow;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use lattice_completion::candidate::RawCandidate;
use lattice_grammar::args::ArgSpec;
use tokio::sync::mpsc;

use crate::RoutingPayload;
use crate::context::PickerContext;
use crate::outcome::PickerAcceptOutcome;

/// Static metadata describing one picker source.
///
/// `id` is the stable name the user types after `:picker`
/// (e.g. `files`, `lsp-references`). `doc` is one line shown
/// in `:describe-picker` and next to the id in cmdline
/// completion. `args_schema` describes positional args after
/// the source id — same `ArgSpec` machinery the rest of the
/// grammar uses, so `:picker grep <pat> <Tab>` completes
/// through the existing `gen:*` source plumbing.
#[derive(Debug, Clone)]
pub struct PickerSourceSpec {
    /// PL8.F: `Cow<'static, str>` — builtins pass zero-cost `Cow::Borrowed`
    /// literals; a plugin source (crossing WIT) passes `Cow::Owned` that frees
    /// on `PickerRegistry::unregister`, replacing the old `Box::leak` intern.
    pub id: Cow<'static, str>,
    pub doc: Cow<'static, str>,
    pub args_schema: Vec<ArgSpec>,
    /// Parameter-hint line shown while the user is typing args
    /// after the source id. Empty string = no hint (the
    /// cmdline falls back to per-arg `ArgSpec::doc`).
    pub args_hint: Cow<'static, str>,
    /// True if this source re-executes its data fetch on every
    /// (debounced) query change instead of returning a fixed
    /// candidate set the picker fuzzy-filters. Live sources
    /// own their own filtering -- the grep binary IS the
    /// filter -- so the picker bypasses its built-in fuzzy
    /// refilter for them. Sources opting in must also
    /// implement [`PickerSourceGenerator::on_query_changed`].
    pub live: bool,
}

impl PickerSourceSpec {
    /// Sugar for declaring a no-arg picker source (`files`,
    /// `recent`, `buffers`, etc.).
    pub fn no_args(id: impl Into<Cow<'static, str>>, doc: impl Into<Cow<'static, str>>) -> Self {
        Self {
            id: id.into(),
            doc: doc.into(),
            args_schema: Vec::new(),
            args_hint: Cow::Borrowed(""),
            live: false,
        }
    }

    /// Builder-style: mark this source as live (`:picker grep`
    /// today; future live LSP workspace-symbols, etc.). The
    /// picker will bypass its fuzzy refilter for live sources
    /// and the host will call
    /// [`PickerSourceGenerator::on_query_changed`] on each
    /// debounced keystroke.
    pub fn with_live(mut self, live: bool) -> Self {
        self.live = live;
        self
    }
}

/// Registry of every picker source the `:picker <id>` ex-command
/// can dispatch to. Populated at boot by each feature crate's
/// `register_picker_sources` entry point.
///
/// Re-registering an id overwrites the previous entry — last
/// writer wins. In practice each id is registered exactly once
/// at boot; the overwrite semantics make tests trivial to write
/// (`register` twice with different specs to assert the second
/// wins).
/// `Clone` so the host can hold the registry behind an `Arc<ArcSwap<_>>` and
/// register plugin sources at runtime by copy-on-write RCU (clone → mutate →
/// store) — the same wait-free-read / rare-write idiom the snippet registry
/// uses. Reads stay lock-free; a plugin load / unload swaps a fresh registry
/// in. Cloning is cheap: the sources map holds `&'static str` keys and
/// `Arc`-shared generators.
#[derive(Debug, Default, Clone)]
pub struct PickerRegistry {
    // PL8.F: keyed by `Cow<'static, str>` (was `&'static str`) so a plugin
    // source's owned id frees on `unregister` — lookups still take `&str` via
    // `Cow: Borrow<str>`.
    sources: HashMap<Cow<'static, str>, RegistryEntry>,
}

/// The runtime-mutable registry handle the editor holds and shares as a service.
/// `ArcSwap` gives wait-free reads on the picker-open path and copy-on-write RCU
/// writes for runtime plugin-source registration (`:plugin-load` /
/// boot-discovery): clone the current registry, `register_generator` /
/// `unregister`, then `store` the new snapshot. The plugin loader
/// (`lattice-plugin-loader`) reaches this via `service::<PickerRegistryHandle>()`
/// and RCU-registers each loaded picker plugin's `WasmPickerSource`.
pub type PickerRegistryHandle = std::sync::Arc<arc_swap::ArcSwap<PickerRegistry>>;

/// Registry entry: either metadata-only (slice 12 path,
/// retained for tab-completion of source ids whose generator
/// isn't yet wired) or metadata + generator (the canonical
/// slice 13 path -- dispatch resolves the generator from
/// here).
///
/// Metadata-only entries land while the App still owns the
/// imperative `open_*_picker` dispatch table; full
/// generator entries flow through the trait-driven path. As
/// each source migrates to a `PickerSourceGenerator` impl
/// the registry transitions from metadata-only to full.
#[derive(Clone)]
pub struct RegistryEntry {
    pub spec: PickerSourceSpec,
    pub generator: Option<std::sync::Arc<dyn PickerSourceGenerator>>,
}

impl std::fmt::Debug for RegistryEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RegistryEntry")
            .field("spec", &self.spec.id)
            .field("has_generator", &self.generator.is_some())
            .finish()
    }
}

impl PickerRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a metadata-only entry. Used for sources whose
    /// imperative App-side `open_*_picker` method still drives
    /// dispatch (slice 12 path). Migrates to
    /// [`Self::register_generator`] when the source's
    /// `PickerSourceGenerator` impl lands.
    pub fn register(&mut self, spec: PickerSourceSpec) {
        let id = spec.id.clone();
        self.sources.insert(
            id,
            RegistryEntry {
                spec,
                generator: None,
            },
        );
    }

    /// Register a source with both metadata and a generator
    /// trait object. The spec is read from `generator.spec()`
    /// so callers don't repeat themselves. Canonical path for
    /// fully trait-driven sources.
    pub fn register_generator(&mut self, generator: std::sync::Arc<dyn PickerSourceGenerator>) {
        let spec = generator.spec().clone();
        let id = spec.id.clone();
        self.sources.insert(
            id,
            RegistryEntry {
                spec,
                generator: Some(generator),
            },
        );
    }

    /// Remove a registered source by its id, the teardown seam for a plugin
    /// reload / unload (PH7.12b). The registry keys on `spec.id` and carries no
    /// plugin-id provenance, so removal is by id — the host's teardown bundle
    /// records the id it registered and drives this. Idempotent: `false` if no
    /// source was registered under `id` (a second unload, or a never-registered
    /// id). Without it a reload would leave the previous generator's dead entry
    /// behind (calls fail `PluginGone`) and the interned id string keeps
    /// leaking across reloads (audit F6, freed by the per-plugin pool in
    /// PH7.12b.2).
    pub fn unregister(&mut self, id: &str) -> bool {
        let before = self.sources.len();
        self.sources.retain(|k, _| *k != id);
        self.sources.len() != before
    }

    pub fn get(&self, id: &str) -> Option<&PickerSourceSpec> {
        self.sources.get(id).map(|e| &e.spec)
    }

    /// Borrow the full registry entry (metadata + generator
    /// slot). Used by the dispatcher to fetch both pieces at
    /// once during the migration window.
    pub fn entry(&self, id: &str) -> Option<&RegistryEntry> {
        self.sources.get(id)
    }

    /// Look up the registered generator for a source id.
    /// `None` if the id isn't registered, or if it's a
    /// metadata-only entry (slice 12 / pre-migration source).
    pub fn generator(&self, id: &str) -> Option<&std::sync::Arc<dyn PickerSourceGenerator>> {
        self.sources.get(id).and_then(|e| e.generator.as_ref())
    }

    /// Walk every registered source in id-sorted order.
    /// Deterministic for tab-completion and `:describe-picker`
    /// listings; tests can rely on the order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &PickerSourceSpec)> + '_ {
        // PL8.F: keys are `Cow` now — borrow each as `&str` (was `.copied()` on
        // `&'static str` keys). `self.sources[id]` still indexes via
        // `Cow: Borrow<str>`.
        let mut ids: Vec<&str> = self.sources.keys().map(|k| k.as_ref()).collect();
        ids.sort_unstable();
        ids.into_iter().map(move |id| (id, &self.sources[id].spec))
    }

    pub fn ids(&self) -> impl Iterator<Item = &str> + '_ {
        let mut ids: Vec<&str> = self.sources.keys().map(|k| k.as_ref()).collect();
        ids.sort_unstable();
        ids.into_iter()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }
}

/// Source-side fallible result. Errors are user-facing
/// strings the host echoes verbatim. Wrapping in
/// `Result<...,String>` rather than a typed error keeps the
/// WIT mirror (Phase 7) trivial -- plugin-emitted errors
/// cross the boundary as strings.
pub type SourceResult<T> = Result<T, String>;

/// One batch of `(candidate, routing payload)` pairs from a
/// source. Identical shape across all three init flavors;
/// the host's seat / append paths use it uniformly.
pub type CandidateBatch = Vec<(RawCandidate, RoutingPayload)>;

/// One-shot async future a source returns when the
/// candidate set requires off-thread work (LSP request,
/// large directory walk, etc.). `'static + Send` because
/// the source extracted everything it needs from the
/// context during the synchronous `init` call and moved it
/// into the closure.
pub type CandidateFuture = Pin<Box<dyn Future<Output = SourceResult<CandidateBatch>> + Send>>;

/// Streaming source channel. Sources spawn a producer task
/// during `init` and return the receiver; the host pumps
/// each batch into the picker incrementally so the popup
/// updates as results arrive (live-grep, future
/// live-LSP-completion).
pub type CandidateStream = mpsc::UnboundedReceiver<SourceResult<CandidateBatch>>;

/// One-shot async future a source returns from [`accept_async`] when
/// translating the chosen routing payload requires off-thread work — the
/// motivating case is a WASM plugin source whose `accept` is an async guest
/// call bound to its actor task (`lattice-plugin-host`). Same `'static + Send`
/// contract as [`CandidateFuture`]: the source extracts everything it needs
/// (the projected context, the routing token) during the synchronous
/// `accept_async` prelude and moves it into the closure.
///
/// [`accept_async`]: PickerSourceGenerator::accept_async
pub type AcceptFuture = Pin<Box<dyn Future<Output = SourceResult<PickerAcceptOutcome>> + Send>>;

/// Three init shapes covering every Phase 4-8 picker
/// pattern. The choice is per-invocation -- a single source
/// can return different shapes depending on args (e.g. a
/// future grep source could `Inline` an empty result on
/// empty pattern, `Stream` otherwise).
pub enum PickerInitResult {
    /// Sync sources (files, recent, lines, marks, registers,
    /// jumps, commands, snippets, tree-sitter outline).
    Inline(CandidateBatch),
    /// One-shot async (LSP references / definitions /
    /// symbols / code actions / diagnostics snapshot).
    Future(CandidateFuture),
    /// Multi-batch streaming (live-grep subprocess, future
    /// live LSP completion).
    Stream(CandidateStream),
}

impl std::fmt::Debug for PickerInitResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PickerInitResult::Inline(batch) => {
                f.debug_struct("Inline").field("len", &batch.len()).finish()
            }
            PickerInitResult::Future(_) => f.debug_struct("Future").finish_non_exhaustive(),
            PickerInitResult::Stream(_) => f.debug_struct("Stream").finish_non_exhaustive(),
        }
    }
}

/// Source generators implement this trait. Registered into
/// [`PickerRegistry`] at boot; the `:picker <source>`
/// dispatcher looks up by `spec().id` and calls `init` to
/// obtain candidates, then `accept` to translate the user's
/// chosen routing payload into a typed outcome.
///
/// **Lifetime story.** `init` and `accept` borrow `&self`
/// (so the generator must be `Sync`) and the
/// `PickerContext<'_>` for the duration of the synchronous
/// call. The borrow is released the moment the method
/// returns; any captured async work owns its own clones
/// (extract URIs, positions, Arc handles, etc. into the
/// closure during the sync prelude).
///
/// **Send + Sync.** The registry stores generators as
/// `Arc<dyn PickerSourceGenerator>` and shares them across
/// the App + tokio task threads. Both bounds are required.
pub trait PickerSourceGenerator: Send + Sync {
    /// Generator metadata. Returned by reference so the
    /// registry can stamp it into `:describe-picker` /
    /// `:picker <Tab>` listings without cloning.
    fn spec(&self) -> &PickerSourceSpec;

    /// Build the candidate set. Sync prelude: read what's
    /// needed from `ctx`, clone into async captures if
    /// necessary, return the appropriate `PickerInitResult`
    /// variant. Synchronous errors (no active buffer when
    /// one was required, args validation failure) return
    /// `Err`; the host echoes the error and leaves the
    /// picker closed.
    ///
    /// `args` is the tokens the user typed after the source
    /// id (`:picker files /tmp/foo` -> `&["/tmp/foo"]`).
    /// Sources interpret them against their declared
    /// [`PickerSourceSpec::args_schema`]; the grammar layer
    /// doesn't pre-validate because per-source arg shapes
    /// vary too much.
    fn init(&self, ctx: &PickerContext<'_>, args: &[String]) -> SourceResult<PickerInitResult>;

    /// Translate the user's chosen routing payload into a
    /// typed `PickerAcceptOutcome` the host applies. The
    /// generator owns the mapping from its emitted
    /// routing-payload variant(s) to outcome(s); mismatch
    /// returns `Err`, which the host echoes.
    fn accept(
        &self,
        ctx: &PickerContext<'_>,
        routing: &RoutingPayload,
    ) -> SourceResult<PickerAcceptOutcome>;

    /// Async-accept hook. Default `None` means "this source resolves `accept`
    /// synchronously" — every native source takes this path and is unchanged.
    ///
    /// A source whose accept translation requires off-thread work returns
    /// `Some(future)`; the host spawns it (never blocking the actor thread) and
    /// applies the resolved outcome via the pending-accept drain, exactly the
    /// way [`init`](Self::init)'s [`PickerInitResult::Future`] is drained. The
    /// motivating case is a WASM plugin source (`lattice-plugin-host`): its
    /// guest `accept` export is async and bound to the plugin's actor task, so
    /// there is no synchronous path from the keystroke to plugin code
    /// (paramount #4) — a slow/hostile plugin accept can never freeze the UI.
    ///
    /// Like `init`, this is a **synchronous prelude**: read what's needed from
    /// `ctx` + `routing`, project/clone into the returned `'static` future, and
    /// release the borrows. A source that returns `Some` here still implements
    /// the sync [`accept`](Self::accept) (the host prefers `accept_async` when
    /// it returns `Some`, so that body is the fallback / tripwire only).
    fn accept_async(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> Option<AcceptFuture> {
        None
    }

    /// Live-source hook: re-fetch candidates when the user's
    /// query changes. Default `None` means "static source --
    /// host uses fuzzy refilter over the candidate set
    /// returned by `init`". Returning `Some(result)` from a
    /// source whose `spec().live == true` replaces the
    /// picker's candidate set wholesale; the host wires up
    /// debouncing and cancels any in-flight invocation when a
    /// fresh query arrives.
    ///
    /// The host calls this only after the picker has been
    /// seated by `init`. Sources that opt in MUST set
    /// `spec().live = true`; the two declarations stay
    /// paired because the picker decides whether to bypass
    /// fuzzy-refilter purely from `spec().live`, while the
    /// host decides whether to invoke this hook from the same
    /// flag.
    fn on_query_changed(
        &self,
        _ctx: &PickerContext<'_>,
        _query: &str,
    ) -> Option<SourceResult<PickerInitResult>> {
        None
    }

    /// T.12: live-preview hook — invoked as the picker SELECTION moves to
    /// a candidate (before accept). Default None = no preview. A source
    /// returns an outcome the host applies immediately for a live preview;
    /// the host restores prior state on <Esc>.
    ///
    /// **This runs on the editor's actor thread, synchronously.** Read
    /// what is already in hand and return; a source that must *do*
    /// something to answer (spawn git, read a file, walk an index)
    /// declares [`preview_debounce`](Self::preview_debounce) so the host
    /// asks only once the selection has settled.
    fn preview(
        &self,
        _ctx: &PickerContext<'_>,
        _routing: &RoutingPayload,
    ) -> Option<crate::outcome::PickerPreviewOutcome> {
        None
    }

    /// MG.54: how long the selection must sit still before the host
    /// calls [`preview`](Self::preview). `None` (the default) = call it
    /// inline on every selection move, which is right for a source whose
    /// preview is a cheap projection of data it already holds.
    ///
    /// **What this buys is not a faster call — it is no call at all.**
    /// Arrowing through candidates restarts the timer; a source that
    /// spawns a subprocess to answer therefore spawns ZERO of them while
    /// the user is scrolling, and exactly one when they stop. That is
    /// what makes a synchronous, subprocess-backed preview viable: there
    /// is never an in-flight call to cancel and never a stale result to
    /// race, because the only call ever made is for the candidate the
    /// user is already sitting on.
    ///
    /// The residual cost is real and deliberate: a keystroke arriving
    /// while the settled fetch is running waits for it. Bounded to one
    /// fetch per settle, never a queue. A source declaring this owes its
    /// user an option to turn the feature off, and a guard on how much
    /// work the fetch can be.
    fn preview_debounce(&self) -> Option<std::time::Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    fn spec(id: &'static str) -> PickerSourceSpec {
        PickerSourceSpec::no_args(id, "test source")
    }

    #[test]
    fn new_registry_is_empty() {
        let reg = PickerRegistry::new();
        assert!(reg.is_empty());
        assert_eq!(reg.len(), 0);
        assert!(reg.get("files").is_none());
    }

    #[test]
    fn register_and_get_round_trip() {
        let mut reg = PickerRegistry::new();
        reg.register(spec("files"));
        assert_eq!(reg.len(), 1);
        let got = reg.get("files").unwrap();
        assert_eq!(got.id, "files");
        assert_eq!(got.doc, "test source");
    }

    #[test]
    fn iter_yields_sources_in_id_order() {
        let mut reg = PickerRegistry::new();
        reg.register(spec("recent"));
        reg.register(spec("buffers"));
        reg.register(spec("files"));
        reg.register(spec("lines"));
        let ids: Vec<&str> = reg.iter().map(|(id, _)| id).collect();
        assert_eq!(ids, vec!["buffers", "files", "lines", "recent"]);
    }

    #[test]
    fn re_registering_same_id_overwrites_previous_entry() {
        let mut reg = PickerRegistry::new();
        reg.register(PickerSourceSpec::no_args("files", "first"));
        reg.register(PickerSourceSpec::no_args("files", "second"));
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.get("files").unwrap().doc, "second");
    }

    #[test]
    fn ids_iterator_matches_iter_keys() {
        let mut reg = PickerRegistry::new();
        reg.register(spec("zeta"));
        reg.register(spec("alpha"));
        reg.register(spec("mu"));
        let ids: Vec<&str> = reg.ids().collect();
        assert_eq!(ids, vec!["alpha", "mu", "zeta"]);
    }

    #[test]
    fn unregister_removes_only_the_named_source() {
        let mut reg = PickerRegistry::new();
        reg.register(spec("files"));
        reg.register(spec("recent"));
        assert_eq!(reg.len(), 2);

        // Removes the named source, leaves the other; reports it was present.
        assert!(reg.unregister("files"));
        assert_eq!(reg.len(), 1);
        assert!(reg.get("files").is_none());
        assert!(reg.get("recent").is_some());

        // Idempotent: a second unload (or an unknown id) removes nothing.
        assert!(!reg.unregister("files"));
        assert!(!reg.unregister("never-registered"));
    }

    /// Slice 13a: a no-op test generator that confirms the
    /// trait is object-safe (storable as `Arc<dyn ...>`) and
    /// the `init` / `accept` calling convention compiles. The
    /// generator returns an empty inline batch and a NoOp
    /// outcome; real sources land in slice 13c.
    struct TestGenerator {
        spec: PickerSourceSpec,
    }

    impl PickerSourceGenerator for TestGenerator {
        fn spec(&self) -> &PickerSourceSpec {
            &self.spec
        }

        fn init(
            &self,
            _ctx: &PickerContext<'_>,
            _args: &[String],
        ) -> SourceResult<PickerInitResult> {
            Ok(PickerInitResult::Inline(Vec::new()))
        }

        fn accept(
            &self,
            _ctx: &PickerContext<'_>,
            _routing: &RoutingPayload,
        ) -> SourceResult<PickerAcceptOutcome> {
            Ok(PickerAcceptOutcome::NoOp)
        }
    }

    /// Trait-object usability check. If this compiles, the
    /// trait is object-safe and the registry can hold it.
    #[test]
    fn picker_source_generator_is_object_safe() {
        use std::sync::Arc;

        let g: Arc<dyn PickerSourceGenerator> = Arc::new(TestGenerator {
            spec: PickerSourceSpec::no_args("test", "test generator"),
        });
        assert_eq!(g.spec().id, "test");
    }

    /// `PickerInitResult` Debug doesn't leak the future /
    /// stream internals -- guards against accidental
    /// `Debug` bounds creeping in on `CandidateFuture`.
    #[test]
    fn picker_init_result_debug_is_terse() {
        let inline: PickerInitResult = PickerInitResult::Inline(Vec::new());
        let d = format!("{inline:?}");
        assert!(d.contains("Inline"));
        assert!(d.contains("len: 0"));
    }

    /// Stream + future variant smoke: confirm they at least
    /// construct + Debug without panicking.
    #[test]
    fn picker_init_result_stream_and_future_construct() {
        let (_tx, rx) = mpsc::unbounded_channel();
        let s = PickerInitResult::Stream(rx);
        assert!(format!("{s:?}").contains("Stream"));

        let f: PickerInitResult =
            PickerInitResult::Future(Box::pin(async { Ok::<CandidateBatch, String>(Vec::new()) }));
        assert!(format!("{f:?}").contains("Future"));
    }
}
