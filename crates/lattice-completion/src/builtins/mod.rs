//! Built-in pluggable stage implementations shipped with the
//! editor. Plugin-defined alternatives register through the same
//! [`crate::CompletionRegistry`] and replace these defaults via
//! `default_matcher` / `default_ranker` / `default_annotators`.

pub mod annotators;
pub mod generators;
pub mod matchers;
pub mod rankers;

use crate::registry::{AnnotatorId, CompletionRegistry, GeneratorId, MatcherId, RankerId};

/// Set of ids returned by [`populate`] -- mirrors `Builtins` /
/// `ExBuiltins` in `lattice-grammar`. Lets the host wire defaults
/// from known ids without searching by name.
#[derive(Debug, Clone, Copy)]
pub struct CompletionBuiltins {
    pub gen_commands: GeneratorId,
    pub gen_files: GeneratorId,
    pub match_prefix: MatcherId,
    pub match_substring: MatcherId,
    pub match_fuzzy: MatcherId,
    pub rank_score: RankerId,
    pub rank_alphabetical: RankerId,
    pub anno_kind_label: AnnotatorId,
    pub anno_doc_snippet: AnnotatorId,
}

/// Register the built-ins shipped with the editor and configure
/// sensible defaults: prefix matcher, score ranker, both
/// annotators active.
pub fn populate(registry: &mut CompletionRegistry) -> CompletionBuiltins {
    let gen_commands = registry.register_generator(
        "gen:commands",
        "Every registered command (commands / motions / operators / text-objects / ex-commands).",
        generators::CommandsGenerator,
    );
    let gen_files = registry.register_generator(
        "gen:files",
        "Filesystem entries matching the prefix's directory + basename pattern.",
        generators::FilesGenerator,
    );
    let match_prefix = registry.register_matcher(
        "match:prefix",
        "Exact-prefix match. Fast + predictable.",
        matchers::PrefixMatcher,
    );
    let match_substring = registry.register_matcher(
        "match:substring",
        "Case-insensitive substring contains.",
        matchers::SubstringMatcher,
    );
    let match_fuzzy = registry.register_matcher(
        "match:fuzzy",
        "Subsequence fuzzy match with score-by-density.",
        matchers::FuzzyMatcher,
    );
    let rank_score = registry.register_ranker(
        "rank:score",
        "Descending by matcher score; alphabetical tie-break.",
        rankers::ScoreRanker,
    );
    let rank_alphabetical = registry.register_ranker(
        "rank:alphabetical",
        "Alphabetical (A-Z) on candidate text.",
        rankers::AlphabeticalRanker,
    );
    let anno_kind_label = registry.register_annotator(
        "anno:kind-label",
        "Append `(kind)` after the candidate text -- e.g. `(motion)`, `(file)`.",
        annotators::KindLabelAnnotator,
    );
    let anno_doc_snippet = registry.register_annotator(
        "anno:doc-snippet",
        "Append the first line of the candidate's documentation.",
        annotators::DocSnippetAnnotator,
    );

    // Fuzzy is the v1 default (Q4 from the design discussion --
    // modern editors should have fuzzy matching out of the box).
    // Users / configs can swap to `match:prefix` /
    // `match:substring` via `cmdline.matcher` once §5.12 typed
    // options lands.
    registry.default_matcher = Some(match_fuzzy);
    // Slice `3c.unify.ranker-stack`: default ranker list (chain in
    // registration order). One entry today (`rank_score`); a
    // future `MruRanker` will stack here.
    registry.default_rankers = vec![rank_score];
    registry.default_annotators = vec![anno_kind_label, anno_doc_snippet];

    CompletionBuiltins {
        gen_commands,
        gen_files,
        match_prefix,
        match_substring,
        match_fuzzy,
        rank_score,
        rank_alphabetical,
        anno_kind_label,
        anno_doc_snippet,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn populate_registers_all_builtins() {
        let mut r = CompletionRegistry::new();
        let b = populate(&mut r);
        assert!(r.generator(b.gen_commands).is_some());
        assert!(r.matcher(b.match_prefix).is_some());
        assert!(r.matcher(b.match_fuzzy).is_some());
        assert!(r.ranker(b.rank_score).is_some());
        assert!(r.annotator(b.anno_kind_label).is_some());
    }

    #[test]
    fn populate_sets_sensible_defaults() {
        let mut r = CompletionRegistry::new();
        let b = populate(&mut r);
        // Q4: fuzzy is the v1 default. Configurable via
        // `cmdline.matcher` once typed options land.
        assert_eq!(r.default_matcher, Some(b.match_fuzzy));
        assert_eq!(r.default_rankers, vec![b.rank_score]);
        assert_eq!(
            r.default_annotators,
            vec![b.anno_kind_label, b.anno_doc_snippet]
        );
    }

    #[test]
    fn registered_names_are_introspectable() {
        let mut r = CompletionRegistry::new();
        let _ = populate(&mut r);
        assert!(r.generator_by_name("gen:commands").is_some());
        assert!(r.matcher_by_name("match:fuzzy").is_some());
        assert!(r.ranker_by_name("rank:alphabetical").is_some());
        assert!(r.annotator_by_name("anno:kind-label").is_some());
    }
}
