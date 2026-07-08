//! Autoread — external-change detection + refresh for file-backed
//! Document buffers (vim's `autoread`).
//!
//! See `docs/dev/architecture/autoread.md` for the design and
//! `docs/dev/operations/slice-plans/autoread.md` (AR.*) for sequencing.
//!
//! AR.0 (this file, first slice) lands the **on-disk fingerprint** only —
//! the seam every later slice gates on. No watcher yet. The fingerprint is
//! stamped when a buffer loads and after the editor's own `:w`; the live
//! `notify` watcher (AR.2) compares an incoming filesystem event's post-read
//! fingerprint against the stored one to (a) suppress the event its own save
//! produced and (b) skip no-op `touch`es.

use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::SystemTime;

/// A fast, non-cryptographic hash of a buffer's text. Not stable across
/// process runs (that's fine — fingerprints are session-scoped) and not
/// collision-proof against an adversary (irrelevant — the input is the
/// user's own file, and a collision at worst suppresses one real reload).
fn hash_text(text: &str) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// The on-disk identity of a file-backed buffer at the moment the editor
/// last synced with disk — a load, or its own `:w`.
///
/// Two comparison surfaces, deliberately distinct:
///
/// - [`Self::same_content`] (content hash) is the **authoritative** "is this
///   the same file we already have" test. A `touch` that bumps mtime without
///   changing bytes must compare equal, so mtime/size are *not* part of it.
/// - [`Self::stat_unchanged`] is the cheap `(mtime, size)` **pre-gate** the
///   watcher uses to decide whether it even needs to read + hash the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnDiskFingerprint {
    /// Last-modified time from `stat`, or `None` on platforms / filesystems
    /// where it's unavailable (then detection leans on `content_hash` alone).
    pub mtime: Option<SystemTime>,
    /// Byte length from `stat` (`0` when metadata is unavailable).
    pub size: u64,
    /// Hash of the text the editor holds for this file — the precise check
    /// that survives mtime-only touches and identifies self-writes.
    pub content_hash: u64,
}

impl OnDiskFingerprint {
    /// Build a fingerprint from `path`'s current metadata plus the `text`
    /// the editor holds for it. `stat` failure degrades to
    /// `mtime = None` / `size = 0` rather than erroring — a missing stat
    /// must never break a load or a save (paramount: never panic on the
    /// hot path; recover + lean on the content hash).
    pub fn from_path_and_text(path: &Path, text: &str) -> Self {
        let meta = std::fs::metadata(path).ok();
        let mtime = meta.as_ref().and_then(|m| m.modified().ok());
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        Self {
            mtime,
            size,
            content_hash: hash_text(text),
        }
    }

    /// True when `self` and `other` denote the same on-disk *content*.
    /// Content hash is authoritative; mtime/size are ignored so a bare
    /// `touch` is correctly treated as "no change".
    pub fn same_content(&self, other: &Self) -> bool {
        self.content_hash == other.content_hash
    }

    /// Cheap pre-gate: `true` when `path`'s current `(mtime, size)` still
    /// match this fingerprint, i.e. the file almost certainly hasn't
    /// changed and the watcher can skip the read + hash entirely. A `stat`
    /// failure returns `false` (fall through to the authoritative read),
    /// as does a `None` stored mtime (we never had a baseline to gate on).
    pub fn stat_unchanged(&self, path: &Path) -> bool {
        let Some(stored_mtime) = self.mtime else {
            return false;
        };
        let Ok(meta) = std::fs::metadata(path) else {
            return false;
        };
        meta.len() == self.size && meta.modified().ok() == Some(stored_mtime)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use std::path::PathBuf;

    fn temp_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        std::env::temp_dir().join(format!(
            "lattice-autoread-{}-{}-{}",
            tag,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ))
    }

    #[test]
    fn same_content_ignores_mtime_and_size() {
        // Two fingerprints with identical content hash but different
        // mtime/size compare equal-by-content — a `touch` is not a change.
        let a = OnDiskFingerprint {
            mtime: Some(SystemTime::UNIX_EPOCH),
            size: 10,
            content_hash: hash_text("hello"),
        };
        let b = OnDiskFingerprint {
            mtime: Some(SystemTime::now()),
            size: 999,
            content_hash: hash_text("hello"),
        };
        assert!(a.same_content(&b), "same bytes ⇒ same content");
    }

    #[test]
    fn same_content_differs_on_real_edit() {
        let a = OnDiskFingerprint::from_path_and_text(Path::new("/nonexistent"), "one");
        let b = OnDiskFingerprint::from_path_and_text(Path::new("/nonexistent"), "two");
        assert!(!a.same_content(&b), "different bytes ⇒ different content");
    }

    #[test]
    fn self_write_is_suppressible_by_content_hash() {
        // Simulate: we save text T (stamp F), then read disk back (F').
        // Even though the on-disk mtime moved, F'.same_content(&F) holds,
        // so the watcher can recognise its own write.
        let path = temp_path("selfwrite");
        std::fs::write(&path, "saved text\n").unwrap();
        let stamped = OnDiskFingerprint::from_path_and_text(&path, "saved text\n");
        // A later read of the unchanged file yields the same content hash.
        let reread = OnDiskFingerprint::from_path_and_text(&path, "saved text\n");
        assert!(stamped.same_content(&reread));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn stat_unchanged_true_when_untouched_then_false_after_write() {
        let path = temp_path("stat");
        std::fs::write(&path, "v1\n").unwrap();
        let fp = OnDiskFingerprint::from_path_and_text(&path, "v1\n");
        assert!(fp.stat_unchanged(&path), "freshly stamped ⇒ stat unchanged");

        // Rewrite with different length + (almost certainly) newer mtime.
        std::fs::write(&path, "v2-longer\n").unwrap();
        assert!(
            !fp.stat_unchanged(&path),
            "size/mtime moved ⇒ stat gate opens"
        );
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn stat_unchanged_false_when_no_baseline_mtime_or_missing_file() {
        let no_mtime = OnDiskFingerprint {
            mtime: None,
            size: 0,
            content_hash: 0,
        };
        assert!(!no_mtime.stat_unchanged(Path::new("/nonexistent")));

        let fp = OnDiskFingerprint::from_path_and_text(Path::new("/definitely/missing"), "x");
        assert!(!fp.stat_unchanged(Path::new("/definitely/missing")));
    }
}
