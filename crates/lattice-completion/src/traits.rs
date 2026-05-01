//! The four pluggable traits the pipeline composes.
//!
//! Each is its own extension point. Plugins (post-WASM) register
//! impls against the [`crate::CompletionRegistry`] just like they
//! register commands or keymaps. Default impls ship as built-ins
//! (`crate::builtins::*`).

use std::ops::Range;

use crate::candidate::{CacheKey, MatchScore, RawCandidate, RenderedCandidate, ScoredCandidate};
use lattice_core::Buffer;
use lattice_grammar::CommandRegistry;

/// Snapshot of editor state a generator may need to consult. Held
/// by reference so the pipeline borrows from the surrounding
/// frame for the duration of the call. Generators that don't need
/// buffer or registry state simply ignore those fields.
///
/// `buffer` is the raw rope-backed text the cmdline cursor sits
/// over. We pass `&Buffer` rather than the full `Document` so
/// completion stays decoupled from the actor crate
/// (`lattice-runtime`); generators that need richer document
/// state will get a `&DocumentSnapshot` once a consumer requires
/// one.
pub struct GenerateContext<'a> {
    /// Partial text in the current slot, before the cursor. The
    /// matcher gets the same string as `query` -- generators may
    /// also use it (e.g. `gen:files` resolves the directory by
    /// splitting prefix at the last `/`).
    pub prefix: &'a str,
    pub buffer: &'a Buffer,
    pub registry: &'a CommandRegistry,
    /// Whether matching should be case-sensitive. Generators that
    /// do their own filtering before returning candidates honor
    /// this; pure "produce everything" generators ignore it.
    pub case_sensitive: bool,
}

/// Produces raw candidates for a slot. Implementations vary from
/// pure (`gen:commands` walks the registry) to side-effecting
/// (`gen:files` reads the filesystem). Caching is opt-in via
/// [`Self::cache_key`] -- the default is "no cache".
pub trait CandidateGenerator: Send + Sync {
    fn generate(&self, ctx: &GenerateContext<'_>) -> Vec<RawCandidate>;

    /// Optional cache key. When two contexts produce the same
    /// `Some(key)`, the pipeline serves the second from cache. The
    /// query (prefix) is implicitly part of the key only if the
    /// generator includes it -- many generators key on a registry
    /// version (cache covers all queries until the registry
    /// changes) and let the matcher do the per-query filtering.
    fn cache_key(&self, _ctx: &GenerateContext<'_>) -> Option<CacheKey> {
        None
    }

    /// Soft TTL for cache entries. After elapsing, the entry is
    /// regenerated on next access. Defaults to no expiry.
    fn cache_ttl(&self) -> std::time::Duration {
        std::time::Duration::MAX
    }
}

/// Matches and scores candidates against a query string. The
/// default v1 matcher (`match:prefix`) returns `Some` only when the
/// query is a prefix of `candidate.text`. The `fuzzy` matcher
/// (orderless-equivalent) returns sub-character match ranges and a
/// score that decays with skipped chars.
pub trait CandidateMatcher: Send + Sync {
    /// Score the candidate against `query`. `None` means "no match"
    /// (filter out). The byte ranges record which parts of
    /// `candidate.text` the matcher consumed -- the renderer paints
    /// these with the match-face style.
    fn matches(
        &self,
        query: &str,
        candidate: &RawCandidate,
    ) -> Option<(MatchScore, Vec<Range<usize>>)>;
}

/// Reorders the matched + scored candidate set. The default
/// (`rank:score`) sorts by descending score; `rank:alphabetical`
/// is an alternative. Plugin rankers can implement frecency,
/// recency, smart-case ordering, etc.
pub trait CandidateRanker: Send + Sync {
    fn rank(&self, scored: &mut Vec<ScoredCandidate>);
}

/// Decorates a candidate with display metadata (right-side text in
/// the popup -- marginalia's role). Multiple annotators are active
/// simultaneously; they run in registration order, each appending
/// to `candidate.annotations`. The renderer joins them with `  ` (two
/// spaces) by default.
///
/// Run order is registration order in v1. A future `priority: i32`
/// field will allow plugins to slot themselves between built-in
/// annotators when the registration-order constraint becomes
/// limiting.
pub trait CandidateAnnotator: Send + Sync {
    fn annotate(&self, candidate: &mut RenderedCandidate);
}
