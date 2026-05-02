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

    /// Default matcher used by every pipeline unless overridden
    /// per-slot. User config (post-§5.12) sets this via
    /// `cmdline.matcher = "match:fuzzy"`.
    pub default_matcher: Option<MatcherId>,
    /// Default ranker.
    pub default_ranker: Option<RankerId>,
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
            .field("default_matcher", &self.default_matcher)
            .field("default_ranker", &self.default_ranker)
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
        assert!(r.default_ranker.is_none());
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
}
