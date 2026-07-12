//! Pluggable completion pipeline (DESIGN.md §5.11.3).
//!
//! Powers `:`-line completion and (eventually) any minibuffer-shaped
//! prompt the editor surfaces. Architecturally vertico-shaped:
//! generators, matchers, rankers, annotators are independent
//! pluggable stages registered against a [`CompletionRegistry`].
//! Plugin authors target the public traits to extend any stage.
//!
//! - Generators ([`CandidateGenerator`]) produce raw candidates.
//! - The matcher ([`CandidateMatcher`]) filters + scores them
//!   against a query.
//! - The ranker ([`CandidateRanker`]) reorders the matched set.
//! - Annotators ([`CandidateAnnotator`]) add display metadata.
//!
//! The host (the editor crate, not this one) wires the pipeline
//! into the cmdline and the renderer. This crate stays
//! pure-traits + types + reusable built-ins; tests run without any
//! UI dependency.
//!
//! Source provenance for every registered stage is captured via
//! `#[track_caller]` (DESIGN.md §5.11.1) so
//! `:describe-completion-source <name>` resolves to the registration
//! site.

pub mod builtins;
pub mod cache;
pub mod candidate;
pub mod insert;
pub mod path;
pub mod pipeline;
pub mod registry;
pub mod slot;
pub mod source;
pub mod source_registration;
pub mod traits;

pub use crate::builtins::annotators::{
    DocSnippetAnnotator, KeybindingAnnotator, KeymapReverseLookup, KindLabelAnnotator,
};
pub use crate::builtins::matchers::{
    FuzzyDisplayMatcher, FuzzyMatcher, PrefixMatcher, SubstringMatcher,
};
pub use crate::builtins::rankers::{AlphabeticalRanker, MruRanker, ScoreRanker};
pub use crate::builtins::{CompletionBuiltins, populate};
pub use crate::cache::GeneratorCache;
pub use crate::candidate::{
    Annotation, AnnotationColumns, AnnotationSegment, CacheKey, CandidateData, CandidateKind,
    DisplaySpan, MatchScore, RawCandidate, RenderedCandidate, ScoredCandidate,
};
pub use crate::insert::{
    BufferWordsSource, CompletionTrigger, DocPopupState, FuzzyInsertMatcher, InsertCompletionState,
    InsertContext, InsertRanker, InsertSource, LSP_COMPLETION_SOURCE_ID, PATH_SOURCE_ID,
    PerLanguageOverrides, SNIPPET_SOURCE_ID, SourceId, TREE_SITTER_SYMBOL_SOURCE_ID,
    canonical_source_id, fuzzy_match, per_language_defaults,
};
pub use crate::path::PathCompletionSource;
pub use crate::pipeline::CompletionPipeline;
pub use crate::registry::{
    AnnotatorId, CompletionRegistry, GeneratorId, MatcherId, RankerId, RegisteredAnnotator,
    RegisteredGenerator, RegisteredMatcher, RegisteredRanker,
};
pub use crate::slot::{CommandLineSlot, current_slot};
pub use crate::source::{
    AsyncCompletionSource, CandidateSink, CompletionSourceContribution, CompletionSourceKind,
    InsertContextSnapshot, SyncCompletionSource,
};
pub use crate::source_registration::{
    AcceptAction, AcceptHandler, AcceptToken, ArgsSchema, CandidateSourceKind, CustomAcceptPayload,
    DefaultAcceptHandler, SourceRegistration, SourceSpec,
};
pub use crate::traits::{
    CandidateAnnotator, CandidateGenerator, CandidateMatcher, CandidateRanker, GenerateContext,
};
