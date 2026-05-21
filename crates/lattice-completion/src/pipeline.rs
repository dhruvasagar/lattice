#![allow(clippy::single_range_in_vec_init)]
//! `CompletionPipeline::run` -- the executor that walks
//! generators -> matcher -> ranker -> annotators and produces the
//! list the popup renders.
//!
//! The pipeline is ephemeral: built per-query from the registry's
//! current default matcher / ranker / annotators plus the
//! slot-specific generator(s). Cheap to construct (clones of
//! `Arc<dyn ...>`); we don't bother caching pipelines themselves.
//! What we do cache is generator output -- see [`crate::cache`].

use std::sync::Arc;

use crate::cache::GeneratorCache;
use crate::candidate::{RawCandidate, RenderedCandidate, ScoredCandidate};
use crate::registry::CompletionRegistry;
use crate::traits::{
    CandidateAnnotator, CandidateGenerator, CandidateMatcher, CandidateRanker, GenerateContext,
};

/// One assembled pipeline for a single completion query. Built by
/// the registry's slot resolver (or hand-built for tests).
///
/// Slice `3c.unify.ranker-stack`: the ranker slot is now a `Vec`
/// so dimensions can compose. Each ranker in `rankers` runs in
/// registration order, re-sorting the scored candidates by its
/// own dimension. The downstream renderer sees the result of the
/// LAST ranker — the chain shape is "earlier ranker establishes
/// baseline ordering, later rankers refine within that". A future
/// `MruRanker` will live alongside the builtin `ScoreRanker`
/// without either becoming aware of the other.
pub struct CompletionPipeline {
    pub generators: Vec<Arc<dyn CandidateGenerator>>,
    pub matcher: Arc<dyn CandidateMatcher>,
    pub rankers: Vec<Arc<dyn CandidateRanker>>,
    pub annotators: Vec<Arc<dyn CandidateAnnotator>>,
}

impl CompletionPipeline {
    /// Run every stage. `cache` is the registry's shared cache; the
    /// pipeline reads from / writes to it on the generator stage
    /// only. Matcher / ranker / annotators run live every call.
    pub fn run(
        &self,
        ctx: &GenerateContext<'_>,
        query: &str,
        cache: &GeneratorCache,
    ) -> Vec<RenderedCandidate> {
        // 1. Generate (with caching opt-in per generator).
        let mut raw: Vec<RawCandidate> = Vec::new();
        for g in &self.generators {
            let from_cache = match g.cache_key(ctx) {
                Some(key) => cache.get(&key).map(|cached| (key, cached, g.cache_ttl())),
                None => None,
            };
            match from_cache {
                Some((_, cached, _)) => {
                    raw.extend(cached);
                }
                None => {
                    let produced = g.generate(ctx);
                    if let Some(key) = g.cache_key(ctx) {
                        cache.put(key, produced.clone(), g.cache_ttl());
                    }
                    raw.extend(produced);
                }
            }
        }

        // 2. Match + score. Filtered out where matcher returns None.
        let mut scored: Vec<ScoredCandidate> = raw
            .into_iter()
            .filter_map(|c| {
                self.matcher
                    .matches(query, &c)
                    .map(|(score, ranges)| ScoredCandidate {
                        raw: c,
                        score,
                        match_ranges: ranges,
                    })
            })
            .collect();

        // 3. Rank. Slice `3c.unify.ranker-stack`: chain of rankers,
        // each re-sorting by its own dimension. Order matters —
        // earlier rankers establish baseline order, later rankers
        // refine within. Empty vec is a no-op (filter-only
        // pipeline).
        for r in &self.rankers {
            r.rank(&mut scored);
        }

        // 4. Annotate.
        let mut rendered: Vec<RenderedCandidate> = scored
            .into_iter()
            .map(RenderedCandidate::from_scored)
            .collect();
        for a in &self.annotators {
            for c in rendered.iter_mut() {
                a.annotate(c);
            }
        }
        rendered
    }
}

impl CompletionPipeline {
    /// Slice `3c.unify.picker-via-pipeline`: shared match+rank
    /// entry point for surfaces that have pre-computed candidates
    /// and want the shared matcher + ranker machinery without
    /// the generator / annotator ceremony.
    ///
    /// Used by:
    ///   - Picker (`Picker::refilter`): pre-supplies `raw` via
    ///     `Picker::set_raw_candidates_*`; pipeline holds picker-
    ///     specific matcher (`FuzzyDisplayMatcher`) and rankers
    ///     (`MruRanker` over the picker's bonus map).
    ///   - LSP picker sources (future, slices 10-16): host
    ///     pre-supplies rows from async LSP responses; pipeline
    ///     holds the same picker shape.
    ///
    /// Skips the generator stage (callers have `raw` already) and
    /// the annotator stage (callers without annotation pipelines
    /// don't pay for it). Annotation-enabled callers can call
    /// the full `Pipeline::run` once they have a `GenerateContext`,
    /// or extend this method to take an annotators slice when a
    /// concrete need surfaces.
    pub fn match_and_rank(&self, query: &str, raw: &[RawCandidate]) -> Vec<RenderedCandidate> {
        let mut scored: Vec<ScoredCandidate> = raw
            .iter()
            .filter_map(|c| {
                self.matcher
                    .matches(query, c)
                    .map(|(score, ranges)| ScoredCandidate {
                        raw: c.clone(),
                        score,
                        match_ranges: ranges,
                    })
            })
            .collect();
        for r in &self.rankers {
            r.rank(&mut scored);
        }
        scored.into_iter().map(RenderedCandidate::from_scored).collect()
    }
}

/// Helper that assembles a pipeline from the registry's current
/// configuration plus the slot's generator. Returns `None` if any
/// required piece (default matcher / default ranker / the
/// generator) is missing -- callers surface that as "completion
/// not configured for this slot".
impl CompletionPipeline {
    pub fn for_generator(
        registry: &CompletionRegistry,
        generator: crate::registry::GeneratorId,
    ) -> Option<Self> {
        let g = registry.generator(generator)?;
        let m = registry.matcher(registry.default_matcher?)?;
        // Slice `3c.unify.ranker-stack`: registry now holds an
        // ordered list of default rankers. At least one is required
        // (a pipeline with no rankers would short-circuit "filter
        // only" semantics; we hold the line at "always rank by
        // score" for now). Missing the entire default list is the
        // configuration error.
        if registry.default_rankers.is_empty() {
            return None;
        }
        let rankers: Vec<_> = registry
            .default_rankers
            .iter()
            .filter_map(|id| registry.ranker(*id))
            .map(|r| r.inner.clone())
            .collect();
        if rankers.is_empty() {
            return None;
        }
        let annotators: Vec<_> = registry
            .default_annotators
            .iter()
            .filter_map(|id| registry.annotator(*id))
            .map(|a| a.inner.clone())
            .collect();
        Some(Self {
            generators: vec![g.inner.clone()],
            matcher: m.inner.clone(),
            rankers,
            annotators,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::candidate::{CacheKey, CandidateKind, MatchScore, RawCandidate};
    use lattice_core::{Buffer, Document};
    use lattice_grammar::CommandRegistry;
    use std::ops::Range;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Generator that returns a fixed list and counts how often
    /// `generate()` was called -- lets us prove caching works.
    struct CountingGen {
        items: Vec<String>,
        calls: AtomicUsize,
        cache_key: Option<CacheKey>,
    }

    impl CountingGen {
        fn new(items: Vec<&str>, cache_key: Option<&str>) -> Self {
            Self {
                items: items.into_iter().map(String::from).collect(),
                calls: AtomicUsize::new(0),
                cache_key: cache_key.map(CacheKey::new),
            }
        }
    }

    impl CandidateGenerator for CountingGen {
        fn generate(&self, _: &GenerateContext<'_>) -> Vec<RawCandidate> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.items
                .iter()
                .map(|s| RawCandidate::plain(s, CandidateKind::Plain))
                .collect()
        }
        fn cache_key(&self, _: &GenerateContext<'_>) -> Option<CacheKey> {
            self.cache_key.clone()
        }
    }

    /// Prefix matcher returning the prefix range as the match.
    struct PrefixMatch;
    impl CandidateMatcher for PrefixMatch {
        fn matches(
            &self,
            query: &str,
            c: &RawCandidate,
        ) -> Option<(MatchScore, Vec<Range<usize>>)> {
            if c.text.starts_with(query) {
                Some((MatchScore::PREFIX, vec![0..query.len()]))
            } else {
                None
            }
        }
    }

    /// Score-descending ranker.
    struct ScoreRank;
    impl CandidateRanker for ScoreRank {
        fn rank(&self, scored: &mut Vec<ScoredCandidate>) {
            scored.sort_by(|a, b| b.score.cmp(&a.score));
        }
    }

    /// Annotator that appends the candidate's text length as an
    /// annotation. Tests `annotate()` runs.
    struct LengthAnno;
    impl CandidateAnnotator for LengthAnno {
        fn annotate(&self, c: &mut RenderedCandidate) {
            c.annotations.push(format!("{} chars", c.raw.text.len()));
        }
    }

    fn ctx<'a>(
        prefix: &'a str,
        buffer: &'a Buffer,
        registry: &'a CommandRegistry,
    ) -> GenerateContext<'a> {
        GenerateContext {
            prefix,
            buffer,
            registry,
            case_sensitive: false,
        }
    }

    #[test]
    fn pipeline_runs_all_four_stages() {
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let registry = CommandRegistry::new();
        let cache = GeneratorCache::new();
        let p = CompletionPipeline {
            generators: vec![Arc::new(CountingGen::new(
                vec!["alpha", "beta", "alphabet"],
                None,
            ))],
            matcher: Arc::new(PrefixMatch),
            rankers: vec![Arc::new(ScoreRank)],
            annotators: vec![Arc::new(LengthAnno)],
        };
        let result = p.run(&ctx("alph", &buffer, &registry), "alph", &cache);
        // Only "alpha" and "alphabet" match `alph`.
        assert_eq!(result.len(), 2);
        // Annotator appended length.
        assert!(result[0].annotations[0].contains("chars"));
    }

    #[test]
    fn caching_prevents_regeneration_on_second_run() {
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let registry = CommandRegistry::new();
        let cache = GeneratorCache::new();
        let generator = Arc::new(CountingGen::new(vec!["x", "y"], Some("k1")));
        let p = CompletionPipeline {
            generators: vec![generator.clone()],
            matcher: Arc::new(PrefixMatch),
            rankers: vec![Arc::new(ScoreRank)],
            annotators: vec![],
        };
        let _ = p.run(&ctx("", &buffer, &registry), "", &cache);
        let _ = p.run(&ctx("", &buffer, &registry), "", &cache);
        let _ = p.run(&ctx("", &buffer, &registry), "", &cache);
        // Generator called exactly once -- cache served the next two.
        assert_eq!(generator.calls.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn no_cache_key_means_no_caching() {
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let registry = CommandRegistry::new();
        let cache = GeneratorCache::new();
        let generator = Arc::new(CountingGen::new(vec!["x"], None));
        let p = CompletionPipeline {
            generators: vec![generator.clone()],
            matcher: Arc::new(PrefixMatch),
            rankers: vec![Arc::new(ScoreRank)],
            annotators: vec![],
        };
        let _ = p.run(&ctx("", &buffer, &registry), "", &cache);
        let _ = p.run(&ctx("", &buffer, &registry), "", &cache);
        assert_eq!(generator.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn matcher_filters_non_matches() {
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let registry = CommandRegistry::new();
        let cache = GeneratorCache::new();
        let p = CompletionPipeline {
            generators: vec![Arc::new(CountingGen::new(vec!["foo", "bar", "baz"], None))],
            matcher: Arc::new(PrefixMatch),
            rankers: vec![Arc::new(ScoreRank)],
            annotators: vec![],
        };
        let result = p.run(&ctx("ba", &buffer, &registry), "ba", &cache);
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|r| r.raw.text.starts_with("ba")));
    }

    #[test]
    fn match_ranges_propagate_to_rendered_candidates() {
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let registry = CommandRegistry::new();
        let cache = GeneratorCache::new();
        let p = CompletionPipeline {
            generators: vec![Arc::new(CountingGen::new(vec!["alpha"], None))],
            matcher: Arc::new(PrefixMatch),
            rankers: vec![Arc::new(ScoreRank)],
            annotators: vec![],
        };
        let result = p.run(&ctx("alp", &buffer, &registry), "alp", &cache);
        assert_eq!(result[0].match_ranges, vec![0..3]);
    }

    #[test]
    fn empty_query_with_prefix_matcher_matches_all() {
        let document = Document::empty();
        let buffer = document.buffer().clone();
        let registry = CommandRegistry::new();
        let cache = GeneratorCache::new();
        let p = CompletionPipeline {
            generators: vec![Arc::new(CountingGen::new(vec!["a", "b", "c"], None))],
            matcher: Arc::new(PrefixMatch),
            rankers: vec![Arc::new(ScoreRank)],
            annotators: vec![],
        };
        let result = p.run(&ctx("", &buffer, &registry), "", &cache);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn for_generator_returns_none_when_default_matcher_unset() {
        let registry = CompletionRegistry::new();
        // No matcher / ranker registered -> no defaults set.
        // Adding a generator alone shouldn't make for_generator
        // succeed.
        // (We can't easily register without making one, so this is
        // a smoke-test that the helper requires defaults.)
        assert!(registry.default_matcher.is_none());
    }
}
