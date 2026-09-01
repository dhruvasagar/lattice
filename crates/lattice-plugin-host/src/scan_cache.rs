//! OT.3b — agenda scan results, remembered across restarts.
//!
//! OT.3 priced the parse at 1–2 ms per file (`benches/agenda_scan_input.rs`).
//! An agenda is refreshed far more often than its files change — `gr`,
//! reopening the view, an autocmd, and every editor restart — and without a
//! cache each of those reparses the whole project and re-calls the guest for
//! every file.
//!
//! ## What is cached, and why not the tree
//!
//! The **rows**, not the parse tree. Tree-sitter has no serialisation for a
//! `Tree` — there is no `to_bytes` / `from_bytes` in the crate — so a
//! persistent cache physically cannot hold snapshots. It has to hold what the
//! scan *derived*, which is also what org-roam's database holds for the same
//! reason.
//!
//! That turns out to be the better layer anyway: a hit skips the parse **and**
//! the guest call, where a snapshot cache would only have skipped the parse.
//!
//! ## What it does NOT skip
//!
//! The read. You cannot know a file is unchanged without looking at it, and the
//! host reads it upstream regardless (it needs the text to build the source
//! `Document`). A warm read is ~10–50 µs against a ~2 ms parse, so this is the
//! cheap end and not worth an `mtime` pre-filter's correctness risk.
//!
//! ## The key
//!
//! `(generation, path, content-hash)`.
//!
//! **Content hash, not mtime.** The text is already in hand, so hashing costs
//! ~2–5 µs against the ~2 ms it protects, and it is exactly right: no
//! one-second mtime granularity, no filesystem that lies, no length collision
//! to paper over. A file that came back byte-identical genuinely produces the
//! same rows.
//!
//! **Generation** is the opaque `u64` the guest returns from `begin` — for org,
//! derived from the day the scan is anchored to and the configured TODO
//! keywords. Cached rows embed presentation computed against that anchor
//! (`"tomorrow"`, `"overdue by 2 day(s)"`), so serving them under a different
//! anchor would render yesterday's "tomorrow" as tomorrow — silently wrong at
//! midnight, which is the exact bug `begin`'s anchor exists to prevent. Keying
//! on the generation makes a cached value a pure function of its key, so that
//! cannot happen: the day rolls, the generation changes, the cache is discarded.
//!
//! The host never learns what a date group or a TODO keyword is. It compares
//! two integers.
//!
//! ## Failure behaviour
//!
//! Every failure degrades to "no cache", never to an error and never to a
//! wrong answer: a missing file, a schema-version mismatch, corrupt bytes, an
//! unreadable directory, a failed write. A cache that cannot be trusted is
//! simply not used, and the scan runs as it did before OT.3b.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::scan_task::{ClockSpan, DisplaySpan, Entry};

/// Bumped whenever the on-disk shape changes. A mismatch discards the file
/// rather than attempting migration — this is a cache, and rebuilding it costs
/// one scan.
const SCHEMA_VERSION: u32 = 1;

/// Above this many files the cache is dropped rather than grown without bound.
///
/// Crude on purpose: eviction ORDER does not matter, because a scan repopulates
/// exactly the files it touches on the next pass. The cost of discarding the
/// wrong entry is one parse of a file that was about to be parsed anyway, so an
/// LRU would be bookkeeping bought with nothing.
const MAX_FILES: usize = 4096;

/// One file's cached rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedFile {
    /// Hash of the text these rows were derived from.
    hash: u64,
    rows: Vec<CachedEntry>,
    /// OA.14b: the file's clock spans, cached beside its rows for the reason
    /// `CachedEntry::spans` records — a hit skips the guest call entirely, so
    /// anything left out here is simply absent from a warm scan. A clock
    /// report that is complete on a cold start and lossy on a warm one would
    /// be the least debuggable version of this feature.
    #[serde(default)]
    clock: Vec<CachedClockSpan>,
}

/// The WIT `clock-span`, in a form that survives a round trip to disk. Same
/// reasoning as [`CachedEntry`]: bindgen owns the generated type.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedClockSpan {
    line: u32,
    outline: Vec<String>,
    day: i64,
    minutes: u32,
}

impl From<&ClockSpan> for CachedClockSpan {
    fn from(c: &ClockSpan) -> Self {
        Self {
            line: c.line,
            outline: c.outline.clone(),
            day: c.day,
            minutes: c.minutes,
        }
    }
}

impl From<&CachedClockSpan> for ClockSpan {
    fn from(c: &CachedClockSpan) -> Self {
        Self {
            line: c.line,
            outline: c.outline.clone(),
            day: c.day,
            minutes: c.minutes,
        }
    }
}

/// The WIT `entry`, in a form that survives a round trip to disk.
///
/// A separate type rather than `serde` on the generated WIT struct: bindgen
/// owns that type and regenerates it, so deriving on it would put this file's
/// on-disk compatibility at the mercy of an ABI change it cannot see. The
/// conversion below is the one place the two shapes meet.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedEntry {
    line: u32,
    end_line: u32,
    group: String,
    label: String,
    sort_key: i64,
    /// OA.5: the row's colour, cached with it — a cache hit skips the guest
    /// call entirely, so spans left out here would make a warm agenda render
    /// uncoloured while a cold one rendered correctly.
    ///
    /// `serde(default)` so a cache file written before this field loads
    /// rather than failing: those rows render with no spans until their file
    /// next changes, which is the same "degrade, never break" the rest of
    /// this module takes.
    #[serde(default)]
    spans: Vec<CachedSpan>,
}

/// The WIT `display-span`, in a form that survives a round trip to disk.
/// Same reasoning as [`CachedEntry`]: bindgen owns the generated type.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSpan {
    start: u32,
    end: u32,
    slot: String,
}

impl From<&Entry> for CachedEntry {
    fn from(e: &Entry) -> Self {
        Self {
            line: e.line,
            end_line: e.end_line,
            group: e.group.clone(),
            label: e.label.clone(),
            sort_key: e.sort_key,
            spans: e
                .spans
                .iter()
                .map(|s| CachedSpan {
                    start: s.start,
                    end: s.end,
                    slot: s.slot.clone(),
                })
                .collect(),
        }
    }
}

impl From<&CachedEntry> for Entry {
    fn from(c: &CachedEntry) -> Self {
        Self {
            line: c.line,
            end_line: c.end_line,
            group: c.group.clone(),
            label: c.label.clone(),
            sort_key: c.sort_key,
            spans: c
                .spans
                .iter()
                .map(|s| DisplaySpan {
                    start: s.start,
                    end: s.end,
                    slot: s.slot.clone(),
                })
                .collect(),
        }
    }
}

/// The whole on-disk document.
#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    /// The guest's `begin` generation these rows belong to. A different value
    /// invalidates every entry at once.
    generation: u64,
    files: HashMap<String, CachedFile>,
}

/// A persistent agenda-result cache for one source.
#[derive(Debug)]
pub struct ScanCache {
    path: PathBuf,
    generation: u64,
    files: HashMap<String, CachedFile>,
    /// Entries added since the last flush. Bounds how much a hard kill loses.
    dirty: usize,
    /// Hits and misses, for tests. Asserting on these beats asserting on
    /// timing, which is how you write a flaky test for a cache.
    hits: u64,
    misses: u64,
}

/// Flush after this many new entries, so an unclean exit loses at most this
/// much work rather than a whole scan.
const FLUSH_EVERY: usize = 64;

impl ScanCache {
    /// Open (or start) the cache for `source_id` under `dir`.
    ///
    /// Never fails: an unreadable, corrupt or stale file yields an empty cache
    /// with a `debug` log. `dir` is the host's per-plugin data directory, so
    /// two plugins cannot collide and uninstalling one removes its cache.
    pub fn open(dir: &Path, source_id: u64) -> Self {
        let path = dir.join(format!("scan-cache-{source_id}.json"));
        let loaded = std::fs::read(&path)
            .ok()
            .and_then(|bytes| match serde_json::from_slice::<CacheFile>(&bytes) {
                Ok(file) => Some(file),
                Err(error) => {
                    tracing::debug!(path = %path.display(), %error, "scan cache unreadable; starting empty");
                    None
                }
            })
            .filter(|file| {
                let ok = file.version == SCHEMA_VERSION;
                if !ok {
                    tracing::debug!(
                        path = %path.display(),
                        found = file.version,
                        expected = SCHEMA_VERSION,
                        "scan cache schema mismatch; starting empty"
                    );
                }
                ok
            });
        let (generation, files) = loaded
            .map(|f| (f.generation, f.files))
            .unwrap_or((0, HashMap::new()));
        Self {
            path,
            generation,
            files,
            dirty: 0,
            hits: 0,
            misses: 0,
        }
    }

    /// Start a scan under `generation` (the guest's `begin` return value).
    ///
    /// A changed generation means every cached row was computed against
    /// something that no longer holds — for org, a different day or a different
    /// keyword set — so the whole cache is dropped rather than partially
    /// trusted.
    pub fn begin(&mut self, generation: u64) {
        if self.generation != generation {
            tracing::debug!(
                was = self.generation,
                now = generation,
                files = self.files.len(),
                "scan cache generation changed; discarding"
            );
            self.files.clear();
            self.generation = generation;
            self.dirty = 0;
        }
    }

    /// Cached rows + clock spans for `path`, if the text still hashes the same.
    pub fn get(&mut self, path: &str, text: &str) -> Option<(Vec<Entry>, Vec<ClockSpan>)> {
        let hash = content_hash(text);
        match self.files.get(path) {
            Some(cached) if cached.hash == hash => {
                self.hits += 1;
                Some((
                    cached.rows.iter().map(Entry::from).collect(),
                    cached.clock.iter().map(ClockSpan::from).collect(),
                ))
            }
            _ => {
                self.misses += 1;
                None
            }
        }
    }

    /// Remember what the guest returned for `path`.
    pub fn put(&mut self, path: &str, text: &str, rows: &[Entry], clock: &[ClockSpan]) {
        if self.files.len() >= MAX_FILES && !self.files.contains_key(path) {
            tracing::debug!(cap = MAX_FILES, "scan cache full; discarding");
            self.files.clear();
        }
        self.files.insert(
            path.to_string(),
            CachedFile {
                hash: content_hash(text),
                rows: rows.iter().map(CachedEntry::from).collect(),
                clock: clock.iter().map(CachedClockSpan::from).collect(),
            },
        );
        self.dirty += 1;
        if self.dirty >= FLUSH_EVERY {
            self.flush();
        }
    }

    /// Write the cache out. Best-effort: a failure is logged and dropped,
    /// because a cache that cannot be written is only a slower next scan.
    ///
    /// Written to a temporary file and renamed, so a kill mid-write leaves the
    /// previous cache intact rather than a truncated file the next boot has to
    /// recognise as corrupt.
    pub fn flush(&mut self) {
        self.dirty = 0;
        let file = CacheFile {
            version: SCHEMA_VERSION,
            generation: self.generation,
            files: self.files.clone(),
        };
        let Ok(bytes) = serde_json::to_vec(&file) else {
            tracing::debug!("scan cache failed to serialise; not written");
            return;
        };
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.path.with_extension("json.tmp");
        if let Err(error) = std::fs::write(&tmp, &bytes) {
            tracing::debug!(path = %tmp.display(), %error, "scan cache write failed");
            return;
        }
        if let Err(error) = std::fs::rename(&tmp, &self.path) {
            tracing::debug!(path = %self.path.display(), %error, "scan cache rename failed");
            let _ = std::fs::remove_file(&tmp);
        }
    }

    #[cfg(test)]
    pub(crate) fn stats(&self) -> (u64, u64) {
        (self.hits, self.misses)
    }
}

impl Drop for ScanCache {
    fn drop(&mut self) {
        if self.dirty > 0 {
            self.flush();
        }
    }
}

/// A cheap content fingerprint. Not cryptographic and does not need to be —
/// the input is a file the user just read, and the cost of a collision is one
/// stale agenda row until the next refresh.
fn content_hash(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut hasher);
    hasher.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(line: u32, label: &str) -> Entry {
        Entry {
            line,
            end_line: line,
            group: "g".to_string(),
            label: label.to_string(),
            sort_key: line as i64,
            spans: vec![DisplaySpan {
                start: 2,
                end: 6,
                slot: "keyword".to_string(),
            }],
        }
    }

    /// OA.5: spans round-trip through the on-disk form. A cache hit skips the
    /// guest call entirely, so spans dropped here would make a WARM agenda
    /// render uncoloured while a cold one rendered correctly — a difference
    /// nothing else in the system would explain.
    #[test]
    fn spans_survive_the_cache_round_trip() {
        let e = entry(4, "row");
        let cached = CachedEntry::from(&e);
        let back = Entry::from(&cached);
        assert_eq!(
            back.spans
                .iter()
                .map(|s| (s.start, s.end, s.slot.as_str()))
                .collect::<Vec<_>>(),
            vec![(2, 6, "keyword")]
        );
    }

    /// OA.14b: clock spans round-trip too, for the reason above one step
    /// further on. A hit skips the guest call, so a span left out of the
    /// on-disk form is simply absent from a warm scan — a clock report that is
    /// complete on a cold start and lossy afterwards, with nothing to explain
    /// the difference.
    ///
    /// Driven through the real `put`/`get` rather than the two `From` impls:
    /// the failure this guards is the FIELD being dropped somewhere on the
    /// path, and a conversion test passes happily while `put` ignores its
    /// argument.
    #[test]
    fn clock_spans_survive_the_cache_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = ScanCache::open(dir.path(), 1);
        cache.begin(7);
        let spans = vec![ClockSpan {
            line: 3,
            outline: vec!["Project".to_string(), "Subtask".to_string()],
            day: 20_000,
            minutes: 90,
        }];
        cache.put("/p/a.org", "* TODO a\n", &[entry(0, "Today")], &spans);

        let (rows, clock) = cache.get("/p/a.org", "* TODO a\n").expect("a warm hit");
        assert_eq!(rows.len(), 1, "the rows still come back");
        assert_eq!(
            clock
                .iter()
                .map(|c| (c.line, c.outline.clone(), c.day, c.minutes))
                .collect::<Vec<_>>(),
            vec![(
                3,
                vec!["Project".to_string(), "Subtask".to_string()],
                20_000,
                90
            )],
            "…and so does every field of the clock span, the outline path included"
        );
    }

    #[test]
    fn unchanged_text_hits_and_changed_text_misses() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = ScanCache::open(dir.path(), 1);
        cache.begin(7);

        assert!(cache.get("/p/a.org", "* TODO a\n").is_none());
        cache.put("/p/a.org", "* TODO a\n", &[entry(0, "Today")], &[]);

        let hit = cache.get("/p/a.org", "* TODO a\n").expect("same text hits");
        assert_eq!(hit.0.len(), 1);
        assert_eq!(hit.0[0].label, "Today");

        assert!(
            cache.get("/p/a.org", "* TODO a changed\n").is_none(),
            "different content must not serve the old rows"
        );
    }

    /// The midnight case, and the reason the generation exists at all. Cached
    /// rows carry presentation computed against a day — serving them under a
    /// new one would render yesterday's "tomorrow" as tomorrow.
    #[test]
    fn a_new_generation_discards_everything() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = ScanCache::open(dir.path(), 1);
        cache.begin(7);
        cache.put("/p/a.org", "* TODO a\n", &[entry(0, "tomorrow")], &[]);
        assert!(cache.get("/p/a.org", "* TODO a\n").is_some());

        cache.begin(8);
        assert!(
            cache.get("/p/a.org", "* TODO a\n").is_none(),
            "the day rolled, so every label is suspect"
        );
    }

    /// The whole point of persisting: a restart must not rebuild.
    #[test]
    fn rows_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut cache = ScanCache::open(dir.path(), 1);
            cache.begin(7);
            cache.put("/p/a.org", "* TODO a\n", &[entry(3, "Today")], &[]);
            cache.flush();
        }
        let mut reopened = ScanCache::open(dir.path(), 1);
        reopened.begin(7);
        let hit = reopened
            .get("/p/a.org", "* TODO a\n")
            .expect("a restart reads the cache back");
        assert_eq!(hit.0[0].line, 3);
        assert_eq!(hit.0[0].label, "Today");
    }

    /// Dropping without an explicit flush must still persist — an editor exits
    /// far more often than it calls `flush`.
    #[test]
    fn dropping_the_cache_persists_it() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut cache = ScanCache::open(dir.path(), 1);
            cache.begin(7);
            cache.put("/p/a.org", "* TODO a\n", &[entry(1, "Today")], &[]);
            // no flush() — Drop is the only thing that can save this
        }
        let mut reopened = ScanCache::open(dir.path(), 1);
        reopened.begin(7);
        assert!(reopened.get("/p/a.org", "* TODO a\n").is_some());
    }

    /// Every failure degrades to "no cache", never to a wrong answer.
    #[test]
    fn corrupt_bytes_start_empty_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("scan-cache-1.json"), b"{not json").unwrap();
        let mut cache = ScanCache::open(dir.path(), 1);
        cache.begin(7);
        assert!(cache.get("/p/a.org", "* TODO a\n").is_none());
        // …and it recovers: the next scan repopulates and persists normally.
        cache.put("/p/a.org", "* TODO a\n", &[entry(0, "Today")], &[]);
        assert!(cache.get("/p/a.org", "* TODO a\n").is_some());
    }

    /// A schema bump must not try to read the old shape.
    #[test]
    fn a_schema_mismatch_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let stale = serde_json::json!({
            "version": SCHEMA_VERSION + 1,
            "generation": 7u64,
            "files": {}
        });
        std::fs::write(
            dir.path().join("scan-cache-1.json"),
            serde_json::to_vec(&stale).unwrap(),
        )
        .unwrap();

        let mut cache = ScanCache::open(dir.path(), 1);
        cache.begin(7);
        assert!(cache.get("/p/a.org", "* TODO a\n").is_none());
        let (hits, _) = cache.stats();
        assert_eq!(hits, 0, "nothing from a future schema is trusted");
    }

    /// Two sources must not read each other's rows.
    #[test]
    fn each_source_gets_its_own_file() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut one = ScanCache::open(dir.path(), 1);
            one.begin(7);
            one.put("/p/a.org", "* TODO a\n", &[entry(0, "Today")], &[]);
            one.flush();
        }
        let mut two = ScanCache::open(dir.path(), 2);
        two.begin(7);
        assert!(two.get("/p/a.org", "* TODO a\n").is_none());
    }
}
