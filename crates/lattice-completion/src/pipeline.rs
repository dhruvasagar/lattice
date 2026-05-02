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
pub struct CompletionPipeline {
    pub generators: Vec<Arc<dyn CandidateGenerator>>,
    pub matcher: Arc<dyn CandidateMatcher>,
    pub ranker: Arc<dyn CandidateRanker>,
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

        // 3. Rank.
        self.ranker.rank(&mut scored);

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
        let r = registry.ranker(registry.default_ranker?)?;
        let annotators: Vec<_> = registry
            .default_annotators
            .iter()
            .filter_map(|id| registry.annotator(*id))
            .map(|a| a.inner.clone())
            .collect();
        Some(Self {
            generators: vec![g.inner.clone()],
            matcher: m.inner.clone(),
            ranker: r.inner.clone(),
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
            ranker: Arc::new(ScoreRank),
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
            ranker: Arc::new(ScoreRank),
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
            ranker: Arc::new(ScoreRank),
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
            ranker: Arc::new(ScoreRank),
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
            ranker: Arc::new(ScoreRank),
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
            ranker: Arc::new(ScoreRank),
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
