//! Where pluggable completion stages register themselves.
//!
//! Mirrors the shape of [`lattice_grammar::CommandRegistry`]: each
//! kind of registrant gets a `register_*` method (`#[track_caller]`
//! so source provenance is captured for `:describe-completion-source`)
//! and an internal `pub(crate) insert_*` that the host or trusted
//! subsystems can use with an explicit source.
//!
//! Generators / matchers / rankers / annotators are all looked up
//! by typed id newtype. The host configures one default matcher,
//! one default ranker, and an ordered list of default annotators;
//! per-slot pipelines clone these from the registry at query time.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_grammar::CommandId;
use lattice_grammar::source::SourceLocation;

use crate::cache::GeneratorCache;
use crate::traits::{CandidateAnnotator, CandidateGenerator, CandidateMatcher, CandidateRanker};

/// Strongly-typed handle to a registered completion generator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GeneratorId(pub CommandId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MatcherId(pub CommandId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RankerId(pub CommandId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AnnotatorId(pub CommandId);

/// Metadata + impl for a registered generator. The `inner` is held
/// behind `Arc<dyn ...>` so the pipeline can clone the handle
/// without taking ownership.
pub struct RegisteredGenerator {
    pub id: GeneratorId,
    pub name: String,
    pub doc: String,
    pub source: SourceLocation,
    pub inner: std::sync::Arc<dyn CandidateGenerator>,
}

pub struct RegisteredMatcher {
    pub id: MatcherId,
    pub name: String,
    pub doc: String,
    pub source: SourceLocation,
    pub inner: std::sync::Arc<dyn CandidateMatcher>,
}

pub struct RegisteredRanker {
    pub id: RankerId,
    pub name: String,
    pub doc: String,
    pub source: SourceLocation,
    pub inner: std::sync::Arc<dyn CandidateRanker>,
}

pub struct RegisteredAnnotator {
    pub id: AnnotatorId,
    pub name: String,
    pub doc: String,
    pub source: SourceLocation,
    pub inner: std::sync::Arc<dyn CandidateAnnotator>,
}

#[derive(Default)]
pub struct CompletionRegistry {
    generators: HashMap<GeneratorId, RegisteredGenerator>,
    matchers: HashMap<MatcherId, RegisteredMatcher>,
    rankers: HashMap<RankerId, RegisteredRanker>,
    annotators: HashMap<AnnotatorId, RegisteredAnnotator>,

    /// Slice `3c.unify.source-registration-bundle` (7c). Stores
    /// `SourceRegistration` bundles keyed by their stable id
    /// (`SourceSpec::id`). Picker + cmdline-completion both
    /// look up by id when invoked. First-party sources are
    /// registered at boot; LSP / plugin async-fetch sources
    /// are constructed transiently and passed directly to
    /// `Picker::open_with` (NOT stored here — registry is for
    /// persistent registrations only).
    ///
    /// Slice 7d does the cutover from the parallel
    /// `lattice_picker::PickerRegistry` (`:picker <name>`
    /// lookup → `source_by_id`).
    sources: HashMap<String, crate::source_registration::SourceRegistration>,

    /// Default matcher used by every pipeline unless overridden
    /// per-slot. User config (post-§5.12) sets this via
    /// `cmdline.matcher = "match:fuzzy"`.
    pub default_matcher: Option<MatcherId>,
    /// Default rankers, in chain order. Earlier rankers establish
    /// baseline order; later rankers refine within. Slice
    /// `3c.unify.ranker-stack` replaced `Option<RankerId>` with
    /// `Vec<RankerId>` so dimensions (e.g. `ScoreRanker` then
    /// `MruRanker`) compose without either becoming aware of the
    /// other. Empty list ⇒ no rankers ⇒ `Pipeline::for_generator`
    /// returns `None` (a configuration error).
    pub default_rankers: Vec<RankerId>,
    /// Annotators that run on every candidate, in registration
    /// order. v1 has no priority field; plugins that need a
    /// specific position re-register existing annotators after
    /// their own.
    pub default_annotators: Vec<AnnotatorId>,

    /// Cache backing every generator that opts in via `cache_key`.
    pub cache: GeneratorCache,
}

impl std::fmt::Debug for CompletionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompletionRegistry")
            .field("generators", &self.generators.len())
            .field("matchers", &self.matchers.len())
            .field("rankers", &self.rankers.len())
            .field("annotators", &self.annotators.len())
            .field("sources", &self.sources.len())
            .field("default_matcher", &self.default_matcher)
            .field("default_rankers", &self.default_rankers)
            .field("default_annotators", &self.default_annotators)
            .finish_non_exhaustive()
    }
}

impl CompletionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    // ---- register_* (#[track_caller]) -- the public path ----

    #[track_caller]
    pub fn register_generator(
        &mut self,
        name: &str,
        doc: &str,
        generator: impl CandidateGenerator + 'static,
    ) -> GeneratorId {
        let source = capture_builtin_source();
        self.insert_generator(name, doc, std::sync::Arc::new(generator), source)
    }

    #[track_caller]
    pub fn register_matcher(
        &mut self,
        name: &str,
        doc: &str,
        m: impl CandidateMatcher + 'static,
    ) -> MatcherId {
        let source = capture_builtin_source();
        self.insert_matcher(name, doc, std::sync::Arc::new(m), source)
    }

    #[track_caller]
    pub fn register_ranker(
        &mut self,
        name: &str,
        doc: &str,
        r: impl CandidateRanker + 'static,
    ) -> RankerId {
        let source = capture_builtin_source();
        self.insert_ranker(name, doc, std::sync::Arc::new(r), source)
    }

    #[track_caller]
    pub fn register_annotator(
        &mut self,
        name: &str,
        doc: &str,
        a: impl CandidateAnnotator + 'static,
    ) -> AnnotatorId {
        let source = capture_builtin_source();
        self.insert_annotator(name, doc, std::sync::Arc::new(a), source)
    }

    /// Slice `3c.unify.source-registration-bundle` (7c).
    /// Register a [`crate::SourceRegistration`] bundle —
    /// substrate for picker + cmdline-completion + plugin
    /// sources. Keys on [`SourceSpec::id`](
    /// crate::source_registration::SourceSpec::id); a
    /// duplicate id overwrites the previous entry (last
    /// registration wins, matching the convention that user
    /// init.rs overrides built-ins).
    ///
    /// No host wiring in 7c — 7d's registry cutover migrates
    /// `:picker <name>` lookup from `PickerRegistry` to
    /// [`Self::source_by_id`]. Until then, registrations land
    /// here but nothing consumes them in production code.
    pub fn register_source(&mut self, reg: crate::source_registration::SourceRegistration) {
        let id = reg.spec.id.clone();
        self.sources.insert(id, reg);
    }

    /// Look up a registered source by its id.
    pub fn source_by_id(
        &self,
        id: &str,
    ) -> Option<&crate::source_registration::SourceRegistration> {
        self.sources.get(id)
    }

    /// Iterate all registered sources in HashMap order.
    /// Callers that need deterministic ordering should sort by
    /// `spec.id` themselves; v1 doesn't dictate ordering at
    /// the registry layer.
    pub fn sources(&self) -> impl Iterator<Item = &crate::source_registration::SourceRegistration> {
        self.sources.values()
    }

    /// Number of registered sources.
    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    // ---- pub(crate) insert_* -- explicit-source path for the
    // ----                       trusted subsystems (config loader,
    // ----                       plugin host bridge).

    pub(crate) fn insert_generator(
        &mut self,
        name: &str,
        doc: &str,
        inner: std::sync::Arc<dyn CandidateGenerator>,
        source: SourceLocation,
    ) -> GeneratorId {
        let id = GeneratorId(next_id());
        self.generators.insert(
            id,
            RegisteredGenerator {
                id,
                name: name.to_string(),
                doc: doc.to_string(),
                source,
                inner,
            },
        );
        id
    }

    pub(crate) fn insert_matcher(
        &mut self,
        name: &str,
        doc: &str,
        inner: std::sync::Arc<dyn CandidateMatcher>,
        source: SourceLocation,
    ) -> MatcherId {
        let id = MatcherId(next_id());
        self.matchers.insert(
            id,
            RegisteredMatcher {
                id,
                name: name.to_string(),
                doc: doc.to_string(),
                source,
                inner,
            },
        );
        id
    }

    pub(crate) fn insert_ranker(
        &mut self,
        name: &str,
        doc: &str,
        inner: std::sync::Arc<dyn CandidateRanker>,
        source: SourceLocation,
    ) -> RankerId {
        let id = RankerId(next_id());
        self.rankers.insert(
            id,
            RegisteredRanker {
                id,
                name: name.to_string(),
                doc: doc.to_string(),
                source,
                inner,
            },
        );
        id
    }

    pub(crate) fn insert_annotator(
        &mut self,
        name: &str,
        doc: &str,
        inner: std::sync::Arc<dyn CandidateAnnotator>,
        source: SourceLocation,
    ) -> AnnotatorId {
        let id = AnnotatorId(next_id());
        self.annotators.insert(
            id,
            RegisteredAnnotator {
                id,
                name: name.to_string(),
                doc: doc.to_string(),
                source,
                inner,
            },
        );
        id
    }

    // ---- lookup ----

    pub fn generator(&self, id: GeneratorId) -> Option<&RegisteredGenerator> {
        self.generators.get(&id)
    }
    pub fn matcher(&self, id: MatcherId) -> Option<&RegisteredMatcher> {
        self.matchers.get(&id)
    }
    pub fn ranker(&self, id: RankerId) -> Option<&RegisteredRanker> {
        self.rankers.get(&id)
    }
    pub fn annotator(&self, id: AnnotatorId) -> Option<&RegisteredAnnotator> {
        self.annotators.get(&id)
    }

    pub fn generator_by_name(&self, name: &str) -> Option<&RegisteredGenerator> {
        self.generators.values().find(|g| g.name == name)
    }
    pub fn matcher_by_name(&self, name: &str) -> Option<&RegisteredMatcher> {
        self.matchers.values().find(|m| m.name == name)
    }
    pub fn ranker_by_name(&self, name: &str) -> Option<&RegisteredRanker> {
        self.rankers.values().find(|r| r.name == name)
    }
    pub fn annotator_by_name(&self, name: &str) -> Option<&RegisteredAnnotator> {
        self.annotators.values().find(|a| a.name == name)
    }

    pub fn generator_count(&self) -> usize {
        self.generators.len()
    }
}

fn next_id() -> CommandId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    CommandId::new(NEXT.fetch_add(1, Ordering::Relaxed))
}

#[track_caller]
fn capture_builtin_source() -> SourceLocation {
    let loc = std::panic::Location::caller();
    SourceLocation {
        layer: lattice_grammar::SourceLayer::Builtin,
        kind: lattice_grammar::SourceKind::File {
            path: std::path::PathBuf::from(loc.file()),
            line: Some(loc.line()),
        },
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::candidate::{MatchScore, RawCandidate, RenderedCandidate, ScoredCandidate};
    use crate::traits::GenerateContext;

    struct StubGen;
    impl CandidateGenerator for StubGen {
        fn generate(&self, _: &GenerateContext<'_>) -> Vec<RawCandidate> {
            Vec::new()
        }
    }

    struct StubMatch;
    impl CandidateMatcher for StubMatch {
        fn matches(
            &self,
            _: &str,
            _: &RawCandidate,
        ) -> Option<(MatchScore, Vec<std::ops::Range<usize>>)> {
            None
        }
    }

    struct StubRank;
    impl CandidateRanker for StubRank {
        fn rank(&self, _: &mut Vec<ScoredCandidate>) {}
    }

    struct StubAnno;
    impl CandidateAnnotator for StubAnno {
        fn annotate(&self, _: &mut RenderedCandidate) {}
    }

    #[test]
    fn empty_registry() {
        let r = CompletionRegistry::new();
        assert_eq!(r.generator_count(), 0);
        assert!(r.default_matcher.is_none());
    }

    #[test]
    fn register_returns_id_and_finds_by_name() {
        let mut r = CompletionRegistry::new();
        let id = r.register_generator("gen:test", "doc", StubGen);
        assert!(r.generator(id).is_some());
        assert_eq!(r.generator_by_name("gen:test").map(|g| g.id), Some(id));
    }

    #[test]
    fn distinct_ids_for_distinct_registrations() {
        let mut r = CompletionRegistry::new();
        let a = r.register_generator("a", "", StubGen);
        let b = r.register_generator("b", "", StubGen);
        assert_ne!(a, b);
    }

    #[test]
    fn each_kind_has_independent_namespace() {
        let mut r = CompletionRegistry::new();
        let _g = r.register_generator("x", "", StubGen);
        let _m = r.register_matcher("x", "", StubMatch);
        let _rk = r.register_ranker("x", "", StubRank);
        let _a = r.register_annotator("x", "", StubAnno);
        assert!(r.generator_by_name("x").is_some());
        assert!(r.matcher_by_name("x").is_some());
        assert!(r.ranker_by_name("x").is_some());
        assert!(r.annotator_by_name("x").is_some());
    }

    #[test]
    fn track_caller_records_registration_site() {
        let mut r = CompletionRegistry::new();
        let expected = line!() + 1;
        let id = r.register_generator("gen:caller-test", "", StubGen);
        let g = r.generator(id).unwrap();
        match &g.source.kind {
            lattice_grammar::SourceKind::File {
                path,
                line: Some(line),
            } => {
                assert!(path.to_string_lossy().ends_with("registry.rs"));
                assert_eq!(*line, expected);
            }
            other => panic!("expected File source, got {other:?}"),
        }
    }

    #[test]
    fn default_slots_start_unset() {
        let r = CompletionRegistry::new();
        assert!(r.default_matcher.is_none());
        assert!(r.default_rankers.is_empty());
        assert!(r.default_annotators.is_empty());
    }

    #[test]
    fn default_annotators_can_be_appended() {
        let mut r = CompletionRegistry::new();
        let a1 = r.register_annotator("a1", "", StubAnno);
        let a2 = r.register_annotator("a2", "", StubAnno);
        r.default_annotators.push(a1);
        r.default_annotators.push(a2);
        assert_eq!(r.default_annotators, vec![a1, a2]);
    }

    // ---- Slice 7c: SourceRegistration storage ----

    /// Smoke test: register a SourceRegistration and look it
    /// back up. `Default` is fine for the stub registration
    /// because every field is independent.
    #[test]
    fn register_source_round_trips_by_id() {
        use crate::candidate::{CandidateKind, RawCandidate};
        use crate::source_registration::{CandidateSourceKind, SourceRegistration, SourceSpec};

        let mut r = CompletionRegistry::new();
        assert_eq!(r.source_count(), 0);

        let rows = vec![RawCandidate::plain("hello", CandidateKind::Plain)];
        let reg = SourceRegistration {
            spec: SourceSpec {
                id: "test:smoke".to_string(),
                doc: "smoke test source".to_string(),
                args_schema: None,
                live: false,
            },
            kind: CandidateSourceKind::PreSupplied(std::sync::Arc::new(rows)),
            accept: None,
            matcher_override: None,
            ranker_overrides: Vec::new(),
            annotator_extras: Vec::new(),
        };
        r.register_source(reg);

        assert_eq!(r.source_count(), 1);
        let looked_up = r.source_by_id("test:smoke").expect("must be registered");
        assert_eq!(looked_up.spec.id, "test:smoke");
        assert_eq!(looked_up.spec.doc, "smoke test source");
        assert!(matches!(
            looked_up.kind,
            CandidateSourceKind::PreSupplied(_)
        ));
        assert!(r.source_by_id("nope").is_none());
    }

    /// Duplicate id overwrites — last-write-wins. Matches the
    /// convention that user init.rs can override a built-in
    /// source's behaviour by re-registering under the same id.
    #[test]
    fn register_source_last_write_wins_on_duplicate_id() {
        use crate::source_registration::{CandidateSourceKind, SourceRegistration, SourceSpec};

        let mut r = CompletionRegistry::new();
        let first = SourceRegistration {
            spec: SourceSpec {
                id: "test:dup".to_string(),
                doc: "first".to_string(),
                args_schema: None,
                live: false,
            },
            kind: CandidateSourceKind::PreSupplied(std::sync::Arc::new(Vec::new())),
            accept: None,
            matcher_override: None,
            ranker_overrides: Vec::new(),
            annotator_extras: Vec::new(),
        };
        let second = SourceRegistration {
            spec: SourceSpec {
                id: "test:dup".to_string(),
                doc: "second".to_string(),
                args_schema: None,
                live: true,
            },
            kind: CandidateSourceKind::PreSupplied(std::sync::Arc::new(Vec::new())),
            accept: None,
            matcher_override: None,
            ranker_overrides: Vec::new(),
            annotator_extras: Vec::new(),
        };
        r.register_source(first);
        r.register_source(second);
        assert_eq!(r.source_count(), 1);
        let looked_up = r.source_by_id("test:dup").expect("must be registered");
        assert_eq!(looked_up.spec.doc, "second");
        assert!(looked_up.spec.live);
    }
}
