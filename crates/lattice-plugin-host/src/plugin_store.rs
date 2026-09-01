//! OR.1 — durable, plugin-scoped key/value storage for guests.
//!
//! Design: `docs/dev/architecture/org-roam.md` §4.2. Slice plan:
//! `docs/dev/operations/slice-plans/org-roam.md` OR.1.
//!
//! ## Why the host holds this at all
//!
//! **There is no single guest instance.** `spawn_event_plugin`,
//! `spawn_config_plugin`, `spawn_help_plugin`, `spawn_dashboard_sections` and
//! `instantiate_grammar_plugin` are separate paths, each building its own
//! `wasmtime::Store` with its own linear memory. A plugin that keeps state "in
//! the guest" therefore keeps *N copies, drifting* — and the drift is invisible,
//! because every instance stays internally consistent while answering a
//! different question from its neighbour. Org-roam's picker would offer a note
//! that its own `<CR>` could not open, and nothing anywhere would report an
//! error.
//!
//! So the sharing point has to be host-side. [`PluginStore`] is that point: one
//! per **manifest id**, handed to every `PluginState` built for that id, so a
//! `put` on the event seam is visible to a `get` on the grammar seam
//! immediately — not after a flush, not after a reload.
//!
//! ## What the host knows about the contents
//!
//! Nothing. Keys are guest-chosen strings and values are opaque bytes. The key
//! layout (`nodes`, `n/<id>`, `b/<id>`, `f/<path>` for roam) is the *guest's*
//! schema, and the host never parses, validates or interprets it — which is the
//! same test `org-mode.md` §2 set for the agenda, applied to persistence, where
//! hosts usually start learning schemas.
//!
//! Because keys are never turned into paths, there is no traversal to defend
//! against. `f//etc/passwd` is a key like any other; the on-disk format
//! length-prefixes it into one file.
//!
//! ## Failure policy
//!
//! `scan_cache.rs`'s, promoted from an agenda special case to a primitive:
//! temp-file-and-rename so a kill mid-write leaves the previous state intact, a
//! schema version that refuses an older shape, a size cap, flush-on-drop, and
//! degradation to **empty** on any corruption. Never to a partial read — a
//! store serving bytes that failed a schema check is how one starts serving
//! plausible nonsense.
//!
//! ## Why the on-disk format is hand-rolled
//!
//! The payload is *bytes*, and every serde format in the tree encodes a
//! `Vec<u8>` as an array of integers — a ~90 KB roam blob becomes ~110 KB of
//! per-element tags for nothing. Byte fidelity is the entire job here, so a
//! length-prefixed frame (30 lines, no dependency, exact) is the better fit
//! than a dependency plus the configuration to stop it being wasteful. It also
//! makes the corruption tests say what they mean: a truncated frame is a
//! truncated frame.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A shared handle to one plugin's store. Cloned into every `PluginState` built
/// for that manifest id, which is what makes one writer visible to N readers
/// across separate `wasmtime::Store`s.
pub type PluginStoreHandle = Arc<Mutex<PluginStore>>;

/// File magic, so a file that is not a store is rejected before its bytes are
/// interpreted as lengths.
const MAGIC: &[u8; 8] = b"LTSTORE\x00";

/// Bumped whenever the on-disk shape changes. A mismatch discards the file
/// rather than attempting migration — the store is rebuildable by definition
/// (roam rescans; any other guest re-derives), and a migration path is code
/// that runs once and is wrong forever after.
const SCHEMA_VERSION: u32 = 1;

/// Above this total value size the store is cleared rather than grown without
/// bound. Generous against the reference corpus (roam's whole index is well
/// under 1 MB for 706 files), so reaching it means a guest is using the store
/// for something it was not meant for.
///
/// Crude on purpose, and the same reasoning as `scan_cache`'s: eviction
/// *order* does not matter, because every legitimate user of this store can
/// rebuild what it lost. An LRU would be bookkeeping bought with nothing.
const MAX_TOTAL_BYTES: usize = 16 * 1024 * 1024;

/// Flush after this many mutations, so an unclean exit loses at most this much
/// rather than a whole index. `Drop` covers the clean exit.
const FLUSH_EVERY: usize = 64;

/// One plugin's durable key/value store.
///
/// Construct through [`PluginStore::open`]; share through a
/// [`PluginStoreHandle`]. Never fails to open: an unreadable, corrupt or
/// stale file yields an empty store with a `debug` log.
#[derive(Debug)]
pub struct PluginStore {
    /// The file the in-memory map is persisted to.
    path: PathBuf,
    /// Sorted, so `keys(prefix)` is a range scan rather than a filter over
    /// everything — the difference matters for roam's `b/<id>` family, which
    /// is the largest.
    entries: BTreeMap<String, Vec<u8>>,
    /// Sum of value lengths, maintained incrementally so the size cap does not
    /// cost a walk per `put`.
    bytes: usize,
    /// Bumped on every successful mutation. Persisted, so a reader that
    /// outlives a restart does not mistake a rebuilt store for an unchanged
    /// one.
    generation: u64,
    /// Mutations since the last flush.
    dirty: usize,
}

impl PluginStore {
    /// Open (or start) the store held in `dir` — the plugin's private data
    /// directory, so two plugins cannot collide and uninstalling one removes
    /// what it persisted.
    ///
    /// Never fails. Every unreadable case degrades to an empty store, which the
    /// next write repopulates.
    pub fn open(dir: &Path) -> Self {
        let path = dir.join("plugin-store.bin");
        let (entries, generation) = match std::fs::read(&path) {
            Ok(bytes) => match decode(&bytes) {
                Ok(decoded) => decoded,
                Err(reason) => {
                    tracing::debug!(
                        path = %path.display(),
                        %reason,
                        "plugin store unreadable; starting empty"
                    );
                    (BTreeMap::new(), 0)
                }
            },
            // Absent is the ordinary first-run case, not a problem.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (BTreeMap::new(), 0),
            Err(error) => {
                tracing::debug!(
                    path = %path.display(),
                    %error,
                    "plugin store could not be read; starting empty"
                );
                (BTreeMap::new(), 0)
            }
        };
        let bytes = entries.values().map(Vec::len).sum();
        Self {
            path,
            entries,
            bytes,
            generation,
            dirty: 0,
        }
    }

    /// Store `value` under `key`, bumping [`generation`](Self::generation).
    ///
    /// `Err` only for a value that cannot fit at all; a value that merely does
    /// not fit *alongside what is there* clears the store first and is then
    /// stored, because the alternative — refusing the newest write — leaves a
    /// full store permanently unable to record the thing it is being asked to
    /// remember.
    pub fn put(&mut self, key: &str, value: Vec<u8>) -> Result<(), String> {
        if value.len() > MAX_TOTAL_BYTES {
            return Err(format!(
                "store put refused: {} bytes exceeds the {MAX_TOTAL_BYTES}-byte store cap",
                value.len()
            ));
        }
        let previous = self.entries.get(key).map(Vec::len).unwrap_or(0);
        if self.bytes - previous + value.len() > MAX_TOTAL_BYTES {
            tracing::warn!(
                cap = MAX_TOTAL_BYTES,
                held = self.bytes,
                "plugin store full; discarding it wholesale"
            );
            self.entries.clear();
            self.bytes = 0;
        }
        self.bytes = self.bytes - self.entries.get(key).map(Vec::len).unwrap_or(0) + value.len();
        self.entries.insert(key.to_string(), value);
        self.mutated();
        Ok(())
    }

    /// The bytes under `key`, or `None` when nothing is stored there.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        self.entries.get(key).cloned()
    }

    /// Forget `key`. Deleting what is not there is `Ok` — a retraction that has
    /// already happened is not an error — but it does NOT bump the generation,
    /// because nothing changed and a reader that rebuilt for it would be
    /// rebuilding for nothing.
    pub fn delete(&mut self, key: &str) -> Result<(), String> {
        if let Some(old) = self.entries.remove(key) {
            self.bytes -= old.len();
            self.mutated();
        }
        Ok(())
    }

    /// Keys carrying `prefix`, sorted. `""` lists everything.
    pub fn keys(&self, prefix: &str) -> Vec<String> {
        // A range scan rather than a full filter: the map is sorted, so the
        // matching keys are contiguous.
        self.entries
            .range(prefix.to_string()..)
            .take_while(|(k, _)| k.starts_with(prefix))
            .map(|(k, _)| k.clone())
            .collect()
    }

    /// The mutation counter a reader compares against what it last built from.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Record a mutation and flush if enough have accumulated.
    fn mutated(&mut self) {
        self.generation += 1;
        self.dirty += 1;
        if self.dirty >= FLUSH_EVERY {
            self.flush();
        }
    }

    /// Write the store out. Best-effort: a failure is logged and dropped,
    /// because a store that cannot be written is a store that rebuilds on the
    /// next boot — degraded, not wrong.
    ///
    /// Written to a temporary file and renamed, so a kill mid-write leaves the
    /// previous state intact rather than a truncated file the next boot has to
    /// recognise as corrupt.
    pub fn flush(&mut self) {
        self.dirty = 0;
        let bytes = encode(&self.entries, self.generation);
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = self.path.with_extension("bin.tmp");
        if let Err(error) = std::fs::write(&tmp, &bytes) {
            tracing::debug!(path = %tmp.display(), %error, "plugin store write failed");
            return;
        }
        if let Err(error) = std::fs::rename(&tmp, &self.path) {
            tracing::debug!(path = %self.path.display(), %error, "plugin store rename failed");
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

impl Drop for PluginStore {
    fn drop(&mut self) {
        if self.dirty > 0 {
            self.flush();
        }
    }
}

/// The on-disk frame: magic, version, generation, count, then length-prefixed
/// key/value pairs. Little-endian throughout; a store written on one machine is
/// not expected to be read on another, but a fixed endianness costs nothing and
/// removes the question.
fn encode(entries: &BTreeMap<String, Vec<u8>>, generation: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 + entries.values().map(Vec::len).sum::<usize>());
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
    out.extend_from_slice(&generation.to_le_bytes());
    out.extend_from_slice(&(entries.len() as u64).to_le_bytes());
    for (key, value) in entries {
        out.extend_from_slice(&(key.len() as u32).to_le_bytes());
        out.extend_from_slice(key.as_bytes());
        out.extend_from_slice(&(value.len() as u32).to_le_bytes());
        out.extend_from_slice(value);
    }
    out
}

/// Read a frame back, or say why it could not be. Every error path is a
/// *reason*, not a partial map: a store is discarded wholesale or trusted
/// wholesale, never half-read.
fn decode(bytes: &[u8]) -> Result<(BTreeMap<String, Vec<u8>>, u64), String> {
    let mut cursor = Cursor { bytes, at: 0 };
    if cursor.take(MAGIC.len())? != MAGIC {
        return Err("not a plugin store (bad magic)".into());
    }
    let version = cursor.u32()?;
    if version != SCHEMA_VERSION {
        return Err(format!(
            "schema version {version} (expected {SCHEMA_VERSION})"
        ));
    }
    let generation = cursor.u64()?;
    let count = cursor.u64()?;
    let mut entries = BTreeMap::new();
    for _ in 0..count {
        let key_len = cursor.u32()? as usize;
        let key = std::str::from_utf8(cursor.take(key_len)?)
            .map_err(|_| "key is not UTF-8".to_string())?
            .to_string();
        let value_len = cursor.u32()? as usize;
        let value = cursor.take(value_len)?.to_vec();
        entries.insert(key, value);
    }
    Ok((entries, generation))
}

/// A bounds-checked read head. Every `take` is checked, so a corrupt length
/// prefix produces a reason string rather than a panic on the boot path.
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self.at.checked_add(n).ok_or("length overflow")?;
        let slice = self.bytes.get(self.at..end).ok_or("truncated")?;
        self.at = end;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn open(dir: &Path) -> PluginStore {
        PluginStore::open(dir)
    }

    #[test]
    fn a_value_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        assert!(store.get("nodes").is_none(), "nothing is stored yet");
        store.put("nodes", vec![1, 2, 3]).unwrap();
        assert_eq!(store.get("nodes"), Some(vec![1, 2, 3]));
    }

    /// The whole point of persisting: a restart must not rebuild.
    #[test]
    fn values_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = open(dir.path());
            store.put("n/ABC", b"a node".to_vec()).unwrap();
            store.flush();
        }
        let reopened = open(dir.path());
        assert_eq!(reopened.get("n/ABC"), Some(b"a node".to_vec()));
    }

    /// An editor exits far more often than a guest calls `flush`.
    #[test]
    fn dropping_the_store_persists_it() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = open(dir.path());
            store.put("nodes", b"x".to_vec()).unwrap();
            // no flush() — Drop is the only thing that can save this
        }
        assert_eq!(open(dir.path()).get("nodes"), Some(b"x".to_vec()));
    }

    /// The generation is what a reader on ANOTHER `wasmtime::Store` compares
    /// against, so it has to move on writes and stand still on reads. A
    /// generation that moved on `get` would make every reader rebuild on every
    /// use; one that did not move on `put` would make none of them ever rebuild.
    #[test]
    fn generation_moves_on_mutation_and_not_on_a_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        let start = store.generation();

        store.put("a", vec![1]).unwrap();
        let after_put = store.generation();
        assert!(after_put > start, "a put moves the generation");

        let _ = store.get("a");
        let _ = store.keys("");
        assert_eq!(
            store.generation(),
            after_put,
            "reads must not move the generation"
        );

        store.delete("a").unwrap();
        assert!(
            store.generation() > after_put,
            "a delete moves it too — a retraction is a change readers must see"
        );
    }

    /// Deleting what is not there is not a change, so it must not make every
    /// reader rebuild.
    #[test]
    fn deleting_an_absent_key_is_ok_and_moves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        let before = store.generation();
        store.delete("never-stored").unwrap();
        assert_eq!(store.generation(), before);
    }

    /// A restart must not restart the generation, or a reader that cached
    /// "built from 12" would read a rebuilt store's 3 and conclude nothing had
    /// changed.
    #[test]
    fn the_generation_survives_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let expected = {
            let mut store = open(dir.path());
            store.put("a", vec![1]).unwrap();
            store.put("b", vec![2]).unwrap();
            store.flush();
            store.generation()
        };
        assert_eq!(open(dir.path()).generation(), expected);
    }

    #[test]
    fn keys_lists_a_prefix_sorted() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        store.put("n/c", vec![]).unwrap();
        store.put("n/a", vec![]).unwrap();
        store.put("b/z", vec![]).unwrap();
        store.put("nodes", vec![]).unwrap();

        assert_eq!(store.keys("n/"), vec!["n/a", "n/c"]);
        assert_eq!(store.keys("b/"), vec!["b/z"]);
        assert_eq!(store.keys("").len(), 4, "an empty prefix lists everything");
        assert!(store.keys("zzz").is_empty());
    }

    /// A key is a key, not a path. Nothing derives a filesystem location from
    /// it, so the shapes a path sanitiser would exist to catch are simply data.
    #[test]
    fn a_key_that_looks_like_a_path_is_just_a_key() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = open(dir.path());
            store.put("f/../../etc/passwd", b"opaque".to_vec()).unwrap();
            store.flush();
        }
        assert_eq!(
            open(dir.path()).get("f/../../etc/passwd"),
            Some(b"opaque".to_vec()),
            "it round-trips as data"
        );
        // And nothing outside the store file was created for it.
        let written: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(written.len(), 1, "one file, whatever the keys look like");
    }

    /// Every failure degrades to empty, never to a partial read — a store
    /// serving bytes that failed a check is how one starts serving plausible
    /// nonsense.
    #[test]
    fn corrupt_bytes_start_empty_rather_than_failing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("plugin-store.bin"), b"not a store at all").unwrap();
        let mut store = open(dir.path());
        assert!(store.get("nodes").is_none());
        // …and it recovers: the next write repopulates and persists normally.
        store.put("nodes", vec![9]).unwrap();
        store.flush();
        assert_eq!(open(dir.path()).get("nodes"), Some(vec![9]));
    }

    /// A truncated frame is the shape a kill mid-write would leave if writes
    /// were not atomic. It must not be half-trusted.
    #[test]
    fn a_truncated_frame_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        {
            let mut store = open(dir.path());
            store.put("a", vec![1, 2, 3, 4]).unwrap();
            store.put("b", vec![5, 6, 7, 8]).unwrap();
            store.flush();
        }
        let path = dir.path().join("plugin-store.bin");
        let full = std::fs::read(&path).unwrap();
        std::fs::write(&path, &full[..full.len() - 3]).unwrap();

        let store = open(dir.path());
        assert!(
            store.keys("").is_empty(),
            "a truncated store is discarded whole, not read up to the tear"
        );
    }

    /// A schema bump must not try to read the old shape.
    #[test]
    fn a_schema_mismatch_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut stale = Vec::new();
        stale.extend_from_slice(MAGIC);
        stale.extend_from_slice(&(SCHEMA_VERSION + 1).to_le_bytes());
        stale.extend_from_slice(&7u64.to_le_bytes());
        stale.extend_from_slice(&0u64.to_le_bytes());
        std::fs::write(dir.path().join("plugin-store.bin"), &stale).unwrap();

        let store = open(dir.path());
        assert!(store.keys("").is_empty());
        assert_eq!(store.generation(), 0, "nor is a future generation trusted");
    }

    /// The cap clears wholesale rather than growing without bound or refusing
    /// the newest write.
    #[test]
    fn the_size_cap_clears_the_store() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        let chunk = vec![0u8; 4 * 1024 * 1024];
        for i in 0..4 {
            store.put(&format!("k{i}"), chunk.clone()).unwrap();
        }
        assert_eq!(store.keys("").len(), 4, "16 MiB exactly still fits");

        store.put("overflow", chunk.clone()).unwrap();
        assert_eq!(
            store.keys(""),
            vec!["overflow"],
            "the cap discards wholesale and keeps the newest write"
        );
    }

    /// A value that can never fit is refused rather than clearing a store it
    /// could not then be stored in anyway.
    #[test]
    fn a_value_larger_than_the_whole_store_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        store.put("keep", vec![1]).unwrap();
        let err = store
            .put("huge", vec![0u8; MAX_TOTAL_BYTES + 1])
            .expect_err("a value bigger than the cap cannot be stored");
        assert!(err.contains("exceeds"), "{err}");
        assert_eq!(
            store.get("keep"),
            Some(vec![1]),
            "and the refusal costs nothing that was already there"
        );
    }

    /// Overwriting must not leak the old value's size into the cap accounting —
    /// otherwise a key rewritten often reports a store that is full of one
    /// entry.
    #[test]
    fn overwriting_a_key_does_not_grow_the_accounted_size() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = open(dir.path());
        for _ in 0..8 {
            store.put("nodes", vec![0u8; 4 * 1024 * 1024]).unwrap();
        }
        assert_eq!(
            store.keys(""),
            vec!["nodes"],
            "eight rewrites of one 4 MiB key never trip a 16 MiB cap"
        );
    }
}
