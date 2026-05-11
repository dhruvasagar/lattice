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
    pub id: &'static str,
    pub doc: &'static str,
    pub args_schema: Vec<ArgSpec>,
    /// Parameter-hint line shown while the user is typing args
    /// after the source id. Empty string = no hint (the
    /// cmdline falls back to per-arg `ArgSpec::doc`).
    pub args_hint: &'static str,
}

impl PickerSourceSpec {
    /// Sugar for declaring a no-arg picker source (`files`,
    /// `recent`, `buffers`, etc.).
    pub fn no_args(id: &'static str, doc: &'static str) -> Self {
        Self {
            id,
            doc,
            args_schema: Vec::new(),
            args_hint: "",
        }
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
#[derive(Debug, Default)]
pub struct PickerRegistry {
    sources: HashMap<&'static str, RegistryEntry>,
}

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
        let id = spec.id;
        self.sources
            .insert(id, RegistryEntry { spec, generator: None });
    }

    /// Register a source with both metadata and a generator
    /// trait object. The spec is read from `generator.spec()`
    /// so callers don't repeat themselves. Canonical path for
    /// fully trait-driven sources.
    pub fn register_generator(
        &mut self,
        generator: std::sync::Arc<dyn PickerSourceGenerator>,
    ) {
        let spec = generator.spec().clone();
        let id = spec.id;
        self.sources.insert(
            id,
            RegistryEntry {
                spec,
                generator: Some(generator),
            },
        );
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
    pub fn generator(
        &self,
        id: &str,
    ) -> Option<&std::sync::Arc<dyn PickerSourceGenerator>> {
        self.sources.get(id).and_then(|e| e.generator.as_ref())
    }

    /// Walk every registered source in id-sorted order.
    /// Deterministic for tab-completion and `:describe-picker`
    /// listings; tests can rely on the order.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &PickerSourceSpec)> + '_ {
        let mut ids: Vec<&'static str> = self.sources.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter().map(move |id| (id, &self.sources[id].spec))
    }

    pub fn ids(&self) -> impl Iterator<Item = &'static str> + '_ {
        let mut ids: Vec<&'static str> = self.sources.keys().copied().collect();
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
pub type CandidateFuture =
    Pin<Box<dyn Future<Output = SourceResult<CandidateBatch>> + Send>>;

/// Streaming source channel. Sources spawn a producer task
/// during `init` and return the receiver; the host pumps
/// each batch into the picker incrementally so the popup
/// updates as results arrive (live-grep, future
/// live-LSP-completion).
pub type CandidateStream = mpsc::UnboundedReceiver<SourceResult<CandidateBatch>>;

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
            PickerInitResult::Inline(batch) => f
                .debug_struct("Inline")
                .field("len", &batch.len())
                .finish(),
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
    fn init(
        &self,
        ctx: &PickerContext<'_>,
        args: &[String],
    ) -> SourceResult<PickerInitResult>;

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
        let ids: Vec<&'static str> = reg.iter().map(|(id, _)| id).collect();
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
        let ids: Vec<&'static str> = reg.ids().collect();
        assert_eq!(ids, vec!["alpha", "mu", "zeta"]);
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

        let f: PickerInitResult = PickerInitResult::Future(Box::pin(async {
            Ok::<CandidateBatch, String>(Vec::new())
        }));
        assert!(format!("{f:?}").contains("Future"));
    }
}
