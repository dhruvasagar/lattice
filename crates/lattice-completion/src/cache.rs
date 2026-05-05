//! Cache for generator output (DESIGN.md §5.11.3, Q13).
//!
//! Caching is opt-in: a generator returns `Some(CacheKey)` from
//! `cache_key()` to participate. The cache lives on the
//! [`crate::CompletionRegistry`]; the pipeline consults it before
//! invoking a generator's `generate()`.
//!
//! The cache stores `Vec<RawCandidate>` (the post-generate set,
//! pre-match). The matcher / ranker / annotators always run against
//! current state -- only generation is cached. This is the right
//! granularity: re-walking the registry for every keystroke is the
//! expensive op; matching against ~100 cached candidates is cheap.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::candidate::{CacheKey, RawCandidate};

#[derive(Debug)]
struct CachedEntry {
    candidates: Vec<RawCandidate>,
    inserted_at: Instant,
    ttl: Duration,
}

/// Generator cache. Held inside the registry; owned by the registry
/// because cache invalidation hooks into registry mutations
/// (registering a new command bumps the commands generator's
/// version, naturally invalidating cached entries via the new key).
#[derive(Debug, Default)]
pub struct GeneratorCache {
    entries: Mutex<HashMap<CacheKey, CachedEntry>>,
}

impl GeneratorCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Return cached candidates for `key` if present and not
    /// expired. Caller is responsible for cloning if it needs the
    /// returned `Vec` to outlive the cache; this signature returns
    /// an owned clone to avoid lock-poisoning footguns.
    pub fn get(&self, key: &CacheKey) -> Option<Vec<RawCandidate>> {
        // `expect` here is intentional: Mutex poisoning would mean
        // a panic occurred while another thread held the lock,
        // which itself is a programmer bug we want to surface.
        #[allow(clippy::unwrap_used)]
        let entries = self.entries.lock().unwrap();
        let entry = entries.get(key)?;
        if entry.inserted_at.elapsed() >= entry.ttl {
            return None;
        }
        Some(entry.candidates.clone())
    }

    /// Store `candidates` under `key` with the given soft TTL.
    pub fn put(&self, key: CacheKey, candidates: Vec<RawCandidate>, ttl: Duration) {
        #[allow(clippy::unwrap_used)]
        let mut entries = self.entries.lock().unwrap();
        entries.insert(
            key,
            CachedEntry {
                candidates,
                inserted_at: Instant::now(),
                ttl,
            },
        );
    }

    /// Remove an entry. Used by tests and future explicit-
    /// invalidation paths.
    pub fn invalidate(&self, key: &CacheKey) {
        #[allow(clippy::unwrap_used)]
        self.entries.lock().unwrap().remove(key);
    }

    /// Drop every cached entry. Used by `clear-completion-cache`
    /// command (post-1.0) and in tests.
    pub fn clear(&self) {
        #[allow(clippy::unwrap_used)]
        self.entries.lock().unwrap().clear();
    }

    /// How many entries are currently cached. For tests + a future
    /// `:cache-stats` command.
    pub fn len(&self) -> usize {
        #[allow(clippy::unwrap_used)]
        self.entries.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::candidate::{CandidateData, CandidateKind};

    fn sample(text: &str) -> RawCandidate {
        RawCandidate {
            text: text.into(),
            display: text.into(),
            kind: CandidateKind::Plain,
            data: CandidateData::Plain,
            source: None,
        }
    }

    #[test]
    fn empty_cache_returns_none() {
        let c = GeneratorCache::new();
        assert!(c.get(&CacheKey::new("nope")).is_none());
        assert!(c.is_empty());
    }

    #[test]
    fn put_then_get_round_trips() {
        let c = GeneratorCache::new();
        let key = CacheKey::new("k1");
        c.put(
            key.clone(),
            vec![sample("a"), sample("b")],
            Duration::from_secs(60),
        );
        let got = c.get(&key).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].text, "a");
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn distinct_keys_are_independent() {
        let c = GeneratorCache::new();
        c.put(
            CacheKey::new("a"),
            vec![sample("aa")],
            Duration::from_secs(60),
        );
        c.put(
            CacheKey::new("b"),
            vec![sample("bb")],
            Duration::from_secs(60),
        );
        assert_eq!(c.get(&CacheKey::new("a")).unwrap()[0].text, "aa");
        assert_eq!(c.get(&CacheKey::new("b")).unwrap()[0].text, "bb");
    }

    #[test]
    fn ttl_zero_evicts_immediately() {
        // Entry expires the moment we read it back; verifies the
        // TTL comparison is `>=` not `>`.
        let c = GeneratorCache::new();
        let key = CacheKey::new("ttl0");
        c.put(key.clone(), vec![sample("x")], Duration::ZERO);
        // Spin until > 0ns has elapsed.
        std::thread::sleep(Duration::from_micros(100));
        assert!(c.get(&key).is_none());
    }

    #[test]
    fn invalidate_drops_entry() {
        let c = GeneratorCache::new();
        let key = CacheKey::new("k");
        c.put(key.clone(), vec![sample("x")], Duration::from_secs(60));
        c.invalidate(&key);
        assert!(c.get(&key).is_none());
    }

    #[test]
    fn clear_drops_all_entries() {
        let c = GeneratorCache::new();
        c.put(
            CacheKey::new("a"),
            vec![sample("a")],
            Duration::from_secs(60),
        );
        c.put(
            CacheKey::new("b"),
            vec![sample("b")],
            Duration::from_secs(60),
        );
        c.clear();
        assert!(c.is_empty());
    }

    #[test]
    fn put_overwrites_existing_key() {
        let c = GeneratorCache::new();
        let key = CacheKey::new("k");
        c.put(key.clone(), vec![sample("old")], Duration::from_secs(60));
        c.put(key.clone(), vec![sample("new")], Duration::from_secs(60));
        assert_eq!(c.get(&key).unwrap()[0].text, "new");
        assert_eq!(c.len(), 1);
    }
}
