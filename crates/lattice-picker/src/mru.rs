//! MRU (frecency) index for picker candidates.
//!
//! This module is the picker-owned, source-agnostic ranking
//! signal. Sources never opt in or out -- the picker derives
//! identity from each candidate's [`RoutingPayload`] via
//! [`routing_identity`] and records a frecency entry against
//! `(source_id, identity)` on every accept. Refilter combines
//! the matcher's string score with the frecency bonus so
//! recently-used candidates float to the top within a tier --
//! mirroring vertico + prescient's "most-recent on top" UX
//! that this design is modelled on.
//!
//! ## Identity derivation
//!
//! [`routing_identity`] is a pure function of [`RoutingPayload`].
//! Variants with a stable identity (`OpenFile { path }`,
//! `Buffer { id }`, `InvokeCommand { id }`, `PasteRegister`,
//! `ExpandSnippet`, `JumpToMark`) return `Some(key)`; variants
//! whose payload drifts with edits or is per-request
//! (`JumpInBuffer`, `JumpToLocation`, `LspCompletion`,
//! `LspCodeAction`) return `None`. The picker silently skips
//! `None` candidates for both record + lookup -- those rows
//! never participate in MRU.
//!
//! ## Frecency formula
//!
//! Recency-dominant, frequency tiebreaker:
//!
//! ```text
//! decay = 0.5 ^ (age / half_life)
//! bonus = decay * RECENCY_WEIGHT + ln(use_count + 1) * FREQUENCY_WEIGHT
//! ```
//!
//! Numbers (`RECENCY_WEIGHT = 100.0`, `FREQUENCY_WEIGHT = 10.0`,
//! `DEFAULT_HALF_LIFE = 7 days`) are tunable via typed options
//! (slice 14c). The shape is fixed.
//!
//! ## Persistence
//!
//! Slice 14b adds `save_to` / `load_from` using bincode. This
//! file holds the in-memory shape only; persistence is a thin
//! wrapper around `entries` + a schema version byte.

use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::RoutingPayload;

/// Default LRU cap per `(source_id, identity)` namespace.
/// Above this, the lowest-frecency entry is evicted on each
/// `record` call.
pub const DEFAULT_CAP_PER_NAMESPACE: usize = 1000;

/// Default half-life for the recency decay term. Tuned to
/// "yesterday's choices still rank meaningfully; last week's
/// fade out by half." `picker.mru.recency-half-life` overrides.
pub const DEFAULT_HALF_LIFE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Recency contribution ceiling. The decay term `0.5^(age/HL)`
/// is in `[0, 1]`; multiplied by this constant the recency
/// bonus tops out at ~100 for a just-used entry.
pub const RECENCY_WEIGHT: f64 = 100.0;

/// Frequency contribution weight. `ln(use_count + 1) * 10`
/// adds ~10 at use_count=1, ~23 at use_count=10, ~46 at
/// use_count=100 -- a slow ramp that keeps frequent items
/// sticky without overwhelming recency.
pub const FREQUENCY_WEIGHT: f64 = 10.0;

/// `(source_id, identity)` -- the index key. Source id
/// namespaces the MRU so opening a file via `:picker files`
/// and switching to it via `:picker buffers` count
/// separately. Identity is derived from `RoutingPayload`.
pub type MruKey = (String, String);

/// One MRU record: when the candidate was last accepted and
/// how many times total. Frecency combines both terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MruEntry {
    pub last_used: SystemTime,
    pub use_count: u32,
}

impl MruEntry {
    fn fresh(now: SystemTime) -> Self {
        Self {
            last_used: now,
            use_count: 1,
        }
    }
}

/// Picker-owned MRU index. Stored host-side as
/// `Arc<RwLock<PickerMruIndex>>`; sources never touch it.
/// `record` mutates; `lookup` is read-only.
#[derive(Debug)]
pub struct PickerMruIndex {
    entries: HashMap<MruKey, MruEntry>,
    cap_per_namespace: usize,
}

impl Default for PickerMruIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl PickerMruIndex {
    pub fn new() -> Self {
        Self::with_cap(DEFAULT_CAP_PER_NAMESPACE)
    }

    pub fn with_cap(cap_per_namespace: usize) -> Self {
        Self {
            entries: HashMap::new(),
            cap_per_namespace,
        }
    }

    /// Record one accept of `identity` from source `source_id`.
    /// If the key already exists, bumps `last_used` to `now` and
    /// increments `use_count`. Otherwise creates a fresh entry,
    /// evicting the lowest-frecency entry in the same source
    /// namespace if at cap.
    pub fn record(&mut self, source_id: &str, identity: &str) {
        self.record_at(source_id, identity, SystemTime::now());
    }

    /// Like [`Self::record`] but with a caller-supplied `now`.
    /// Test fixture; production callers use `record` so the
    /// timestamp ordering is monotonic.
    pub fn record_at(&mut self, source_id: &str, identity: &str, now: SystemTime) {
        let key = (source_id.to_string(), identity.to_string());
        if let Some(entry) = self.entries.get_mut(&key) {
            entry.last_used = now;
            entry.use_count = entry.use_count.saturating_add(1);
            return;
        }
        // Cap check: count only entries in the same source
        // namespace so high-traffic sources (files) don't
        // crowd out low-traffic ones (commands).
        let namespace_size = self.entries.keys().filter(|(s, _)| s == source_id).count();
        if namespace_size >= self.cap_per_namespace
            && let Some(victim) = self.lowest_frecency_in_namespace(source_id, now)
        {
            self.entries.remove(&victim);
        }
        self.entries.insert(key, MruEntry::fresh(now));
    }

    /// Look up the MRU entry for `(source_id, identity)`.
    /// `None` means "never accepted" -- the candidate gets a
    /// 0.0 bonus on score combine.
    pub fn lookup(&self, source_id: &str, identity: &str) -> Option<&MruEntry> {
        self.entries
            .get(&(source_id.to_string(), identity.to_string()))
    }

    /// Compute the frecency bonus a candidate identified by
    /// `(source_id, identity)` should receive when scored at
    /// `now`. Returns 0.0 when there's no entry.
    pub fn frecency_bonus(
        &self,
        source_id: &str,
        identity: &str,
        now: SystemTime,
        half_life: Duration,
    ) -> f64 {
        match self.lookup(source_id, identity) {
            Some(entry) => bonus_of(entry, now, half_life),
            None => 0.0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop every recorded entry. Used by tests + the
    /// `picker.mru.persist = false` boot path that doesn't
    /// load a prior cache.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Iterate every entry. Used by persistence (slice 14b)
    /// and `:describe-picker` introspection.
    pub fn iter(&self) -> impl Iterator<Item = (&MruKey, &MruEntry)> + '_ {
        self.entries.iter()
    }

    fn lowest_frecency_in_namespace(&self, source_id: &str, now: SystemTime) -> Option<MruKey> {
        self.entries
            .iter()
            .filter(|(k, _)| k.0 == source_id)
            .min_by(|(_, a), (_, b)| {
                let ba = bonus_of(a, now, DEFAULT_HALF_LIFE);
                let bb = bonus_of(b, now, DEFAULT_HALF_LIFE);
                ba.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(k, _)| k.clone())
    }

    /// Encode + write the index to `path`. Atomic at the
    /// filesystem level: writes to `<path>.tmp` first then
    /// renames into place so a crash mid-write never leaves a
    /// truncated cache. Errors surface as `Err(_)` for the
    /// host to log + retry on the next accept.
    pub fn save_to(&self, path: &Path) -> Result<(), MruPersistError> {
        let persisted = self.to_persisted();
        let bytes = bincode::serde::encode_to_vec(&persisted, bincode::config::standard())
            .map_err(MruPersistError::Encode)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(MruPersistError::Io)?;
        }
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, &bytes).map_err(MruPersistError::Io)?;
        std::fs::rename(&tmp, path).map_err(MruPersistError::Io)?;
        Ok(())
    }

    /// Read + decode an index from `path`. Returns `Ok(None)`
    /// when the file doesn't exist (fresh install). Returns
    /// `Err` for IO or decode failures so the host can decide
    /// whether to discard + start fresh or surface the error.
    /// The default boot policy (slice 14c) is "discard +
    /// start fresh" -- losing MRU is annoying, refusing to
    /// boot is worse.
    pub fn load_from(path: &Path) -> Result<Option<Self>, MruPersistError> {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(MruPersistError::Io(e)),
        };
        let (persisted, _): (PersistedIndex, usize) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::standard())
                .map_err(MruPersistError::Decode)?;
        if persisted.version != PERSIST_VERSION {
            return Err(MruPersistError::VersionMismatch {
                expected: PERSIST_VERSION,
                found: persisted.version,
            });
        }
        Ok(Some(Self::from_persisted(persisted)))
    }

    fn to_persisted(&self) -> PersistedIndex {
        PersistedIndex {
            version: PERSIST_VERSION,
            cap_per_namespace: self.cap_per_namespace as u32,
            entries: self
                .entries
                .iter()
                .map(|(k, e)| PersistedEntry {
                    source_id: k.0.clone(),
                    identity: k.1.clone(),
                    last_used_unix_seconds: e
                        .last_used
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    use_count: e.use_count,
                })
                .collect(),
        }
    }

    fn from_persisted(persisted: PersistedIndex) -> Self {
        let entries: HashMap<MruKey, MruEntry> = persisted
            .entries
            .into_iter()
            .map(|p| {
                (
                    (p.source_id, p.identity),
                    MruEntry {
                        last_used: UNIX_EPOCH + Duration::from_secs(p.last_used_unix_seconds),
                        use_count: p.use_count,
                    },
                )
            })
            .collect();
        Self {
            entries,
            cap_per_namespace: persisted.cap_per_namespace as usize,
        }
    }
}

/// Schema version stamped on the on-disk cache. Bump when
/// the `PersistedIndex` shape changes incompatibly; loaders
/// that see a different version surface
/// `MruPersistError::VersionMismatch` and the host's boot
/// policy discards + starts fresh.
const PERSIST_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
struct PersistedIndex {
    version: u32,
    cap_per_namespace: u32,
    entries: Vec<PersistedEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedEntry {
    source_id: String,
    identity: String,
    /// Seconds since UNIX epoch. SystemTime isn't directly
    /// serde-serializable; this is the platform-neutral
    /// substitute. Pre-1970 entries clamp to 0.
    last_used_unix_seconds: u64,
    use_count: u32,
}

/// Errors from MRU index persistence. The host (slice 14c)
/// decides whether to discard + start fresh, retry, or surface
/// to the user. Default policy: discard on `VersionMismatch` /
/// `Decode`; log + retry on `Io` (write); never block boot.
#[derive(Debug)]
pub enum MruPersistError {
    Io(std::io::Error),
    Encode(bincode::error::EncodeError),
    Decode(bincode::error::DecodeError),
    VersionMismatch { expected: u32, found: u32 },
}

impl std::fmt::Display for MruPersistError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "MRU index io error: {e}"),
            Self::Encode(e) => write!(f, "MRU index encode error: {e}"),
            Self::Decode(e) => write!(f, "MRU index decode error: {e}"),
            Self::VersionMismatch { expected, found } => write!(
                f,
                "MRU index version mismatch: expected v{expected}, found v{found}"
            ),
        }
    }
}

impl std::error::Error for MruPersistError {}

/// Default path the host's boot uses for the MRU cache.
/// `$XDG_CACHE_HOME/lattice/picker-mru.bincode` falling back
/// to `$HOME/.cache/lattice/picker-mru.bincode` on Linux /
/// the platform-appropriate cache dir elsewhere. Returns
/// `None` when no cache directory can be resolved (e.g.
/// sandboxed embedded runs); the host treats this as
/// "persistence disabled" and runs MRU in-memory only.
pub fn default_persist_path() -> Option<std::path::PathBuf> {
    dirs::cache_dir().map(|d| d.join("lattice").join("picker-mru.bincode"))
}

/// Frecency bonus calculation. Pure function on the entry +
/// reference time + decay half-life. Exposed for benches +
/// the App-side snapshot pass.
pub fn bonus_of(entry: &MruEntry, now: SystemTime, half_life: Duration) -> f64 {
    let age = now
        .duration_since(entry.last_used)
        .unwrap_or(Duration::ZERO);
    let decay = (0.5_f64).powf(age.as_secs_f64() / half_life.as_secs_f64().max(1.0));
    let recency = decay * RECENCY_WEIGHT;
    let frequency = (entry.use_count as f64 + 1.0).ln() * FREQUENCY_WEIGHT;
    recency + frequency
}

/// Derive the MRU identity for a routing payload. `None`
/// means "no stable identity" -- the picker correctly skips
/// MRU for these (LSP locations have drifting line/col,
/// LSP code-action / completion indices are per-request).
///
/// **Plugin-custom routing** (Phase 7) returns `None` because
/// the picker primitive doesn't know how to inspect the
/// opaque bytes. Plugins that want MRU must emit one of the
/// canonical variants; this is documented in
/// `docs/dev/architecture/picker.md` § 8.
pub fn routing_identity(payload: &RoutingPayload) -> Option<String> {
    match payload {
        RoutingPayload::OpenFile { path } => Some(format!("file:{}", path.display())),
        RoutingPayload::Buffer { id } => Some(format!("buf:{id}")),
        RoutingPayload::InvokeCommand { id, .. } => Some(format!("cmd:{id}")),
        RoutingPayload::PasteRegister { name } => Some(format!("reg:{name}")),
        RoutingPayload::JumpToMark { name } => Some(format!("mark:{name}")),
        RoutingPayload::ExpandSnippet { id } => Some(format!("snip:{id}")),
        // T.12: theme names are stable identities, so the colorscheme
        // picker gets MRU recency ranking (recently-applied themes
        // float up).
        RoutingPayload::Colorscheme { name } => Some(format!("colorscheme:{name}")),
        // No stable identity for these -- coordinates drift,
        // indices are per-request, LSP-instance entries are
        // ephemeral, and show-message-request actions key into
        // a per-request transient slot.
        RoutingPayload::JumpInBuffer { .. }
        | RoutingPayload::LspLocation { .. }
        | RoutingPayload::LspCompletion { .. }
        | RoutingPayload::LspCodeAction { .. }
        | RoutingPayload::LspCodeLens { .. }
        | RoutingPayload::ColorPresentation { .. }
        | RoutingPayload::LspInstance { .. }
        // AI sessions are ephemeral (start/stop), like LSP instances.
        | RoutingPayload::AiSession { .. }
        // Pending diffs are ephemeral (resolved + gone), so no MRU identity.
        | RoutingPayload::ResolveDiff { .. }
        | RoutingPayload::AcceptShowMessageAction { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn resolve_diff_routing_has_no_mru_identity() {
        // Pending diffs are ephemeral (resolved + gone), so the diff-review
        // picker rows must never accrue MRU recency.
        assert_eq!(
            routing_identity(&RoutingPayload::ResolveDiff {
                primary: 42,
                accept: true,
            }),
            None
        );
    }

    #[test]
    fn routing_identity_returns_some_for_stable_variants() {
        let cases = [
            (
                RoutingPayload::OpenFile {
                    path: PathBuf::from("/tmp/foo.rs"),
                },
                Some("file:/tmp/foo.rs".to_string()),
            ),
            (RoutingPayload::Buffer { id: 7 }, Some("buf:7".to_string())),
            (
                RoutingPayload::InvokeCommand {
                    id: "ex:edit".into(),
                    args: lattice_grammar::args::Args::None,
                },
                Some("cmd:ex:edit".to_string()),
            ),
            (
                RoutingPayload::PasteRegister { name: 'a' },
                Some("reg:a".to_string()),
            ),
            (
                RoutingPayload::JumpToMark { name: 'a' },
                Some("mark:a".to_string()),
            ),
        ];
        for (payload, expected) in cases {
            assert_eq!(routing_identity(&payload), expected);
        }
    }

    #[test]
    fn routing_identity_returns_none_for_drift_variants() {
        let cases = [
            RoutingPayload::JumpInBuffer {
                buffer_id: 1,
                line: 0,
                col: 0,
            },
            RoutingPayload::LspLocation {
                path: PathBuf::from("/tmp/x"),
                line: 0,
                col: 0,
            },
            RoutingPayload::LspCompletion { index: 0 },
            RoutingPayload::LspCodeAction { index: 0 },
        ];
        for payload in cases {
            assert_eq!(routing_identity(&payload), None);
        }
    }

    #[test]
    fn record_then_lookup_round_trips() {
        let mut mru = PickerMruIndex::new();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
        mru.record_at("files", "file:/tmp/a", now);
        let entry = mru.lookup("files", "file:/tmp/a").unwrap();
        assert_eq!(entry.use_count, 1);
        assert_eq!(entry.last_used, now);
        // Re-record bumps use_count and last_used.
        let later = now + Duration::from_secs(60);
        mru.record_at("files", "file:/tmp/a", later);
        let entry = mru.lookup("files", "file:/tmp/a").unwrap();
        assert_eq!(entry.use_count, 2);
        assert_eq!(entry.last_used, later);
    }

    #[test]
    fn frecency_bonus_decays_with_age() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000);
        let entry = MruEntry {
            last_used: now,
            use_count: 1,
        };
        let fresh = bonus_of(&entry, now, DEFAULT_HALF_LIFE);
        let week_later = bonus_of(&entry, now + DEFAULT_HALF_LIFE, DEFAULT_HALF_LIFE);
        let two_weeks_later = bonus_of(&entry, now + 2 * DEFAULT_HALF_LIFE, DEFAULT_HALF_LIFE);
        // Recency component halves each half-life; frequency
        // (ln(2) * 10 ≈ 6.93) stays constant.
        assert!(fresh > week_later);
        assert!(week_later > two_weeks_later);
        // Sanity: a same-instant entry gets ~RECENCY_WEIGHT
        // worth of recency.
        assert!(fresh >= RECENCY_WEIGHT);
    }

    #[test]
    fn frecency_bonus_is_zero_for_missing_entries() {
        let mru = PickerMruIndex::new();
        let bonus = mru.frecency_bonus(
            "files",
            "file:/tmp/missing",
            SystemTime::now(),
            DEFAULT_HALF_LIFE,
        );
        assert_eq!(bonus, 0.0);
    }

    #[test]
    fn namespacing_keeps_source_buckets_separate() {
        let mut mru = PickerMruIndex::new();
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(3_000_000);
        mru.record_at("files", "file:/tmp/a", now);
        // Same identity under a different source is a distinct
        // key -- the file-bucket vs buffer-bucket distinction.
        assert!(mru.lookup("files", "file:/tmp/a").is_some());
        assert!(mru.lookup("buffers", "file:/tmp/a").is_none());
    }

    #[test]
    fn cap_eviction_drops_lowest_frecency_in_namespace() {
        let mut mru = PickerMruIndex::with_cap(2);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(4_000_000);
        // Two entries at t0 + t0+1m, then one at t0 + 2x_half_life
        // -- the oldest decays the most and should evict on a
        // fresh insert.
        mru.record_at("files", "a", t0);
        mru.record_at("files", "b", t0 + Duration::from_secs(60));
        assert_eq!(mru.len(), 2);
        // Triggers eviction in the `files` namespace.
        let later = t0 + 2 * DEFAULT_HALF_LIFE + Duration::from_secs(120);
        mru.record_at("files", "c", later);
        assert_eq!(mru.len(), 2);
        // The oldest (`a`) should be gone.
        assert!(mru.lookup("files", "a").is_none());
        assert!(mru.lookup("files", "b").is_some());
        assert!(mru.lookup("files", "c").is_some());
    }

    #[test]
    fn cap_eviction_does_not_cross_namespaces() {
        let mut mru = PickerMruIndex::with_cap(1);
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(5_000_000);
        mru.record_at("files", "a", t0);
        mru.record_at("commands", "ex:write", t0);
        // Each namespace is at cap; both entries survive
        // because the cap is per-namespace, not global.
        assert_eq!(mru.len(), 2);
        assert!(mru.lookup("files", "a").is_some());
        assert!(mru.lookup("commands", "ex:write").is_some());
    }

    #[test]
    fn clear_drops_everything() {
        let mut mru = PickerMruIndex::new();
        mru.record("files", "x");
        mru.record("commands", "y");
        mru.clear();
        assert!(mru.is_empty());
    }

    /// 14b: save + load round-trip preserves every entry's
    /// source / identity / use_count, and a reload-then-bonus
    /// computation matches what the original index returned.
    /// Allows ±1s tolerance on `last_used` since the on-disk
    /// shape is second-granularity.
    #[test]
    fn persist_round_trip_preserves_entries() {
        let tmp =
            std::env::temp_dir().join(format!("lattice-mru-rt-{}.bincode", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(6_000_000);
        let mut original = PickerMruIndex::new();
        original.record_at("files", "file:/tmp/a", now);
        original.record_at("files", "file:/tmp/a", now + Duration::from_secs(30));
        original.record_at("commands", "cmd:ex:write", now);
        original.save_to(&tmp).expect("save");
        let loaded = PickerMruIndex::load_from(&tmp)
            .expect("load")
            .expect("file exists");
        assert_eq!(loaded.len(), 2);
        let a = loaded.lookup("files", "file:/tmp/a").expect("a");
        assert_eq!(a.use_count, 2);
        let w = loaded.lookup("commands", "cmd:ex:write").expect("w");
        assert_eq!(w.use_count, 1);
        let _ = std::fs::remove_file(&tmp);
    }

    /// 14b: a missing file (fresh install) returns `Ok(None)`
    /// rather than `Err` so the boot path can fall through to
    /// "start with an empty index" without special-casing.
    #[test]
    fn load_missing_file_returns_none() {
        let tmp =
            std::env::temp_dir().join(format!("lattice-mru-nope-{}.bincode", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let result = PickerMruIndex::load_from(&tmp).expect("ok");
        assert!(result.is_none());
    }

    /// 14b: a corrupt cache surfaces `Err(Decode)` so the
    /// boot policy can discard + start fresh.
    #[test]
    fn load_corrupt_file_returns_err() {
        let tmp =
            std::env::temp_dir().join(format!("lattice-mru-bad-{}.bincode", std::process::id()));
        std::fs::write(&tmp, b"definitely not bincode").expect("write");
        let err = PickerMruIndex::load_from(&tmp).unwrap_err();
        assert!(matches!(err, MruPersistError::Decode(_)));
        let _ = std::fs::remove_file(&tmp);
    }

    /// 14b: save uses a tmp-and-rename pattern so the cache
    /// file is never truncated. After a successful save the
    /// `.tmp` sidecar should be gone.
    #[test]
    fn save_atomicity_leaves_no_tmp_sidecar() {
        let tmp =
            std::env::temp_dir().join(format!("lattice-mru-atom-{}.bincode", std::process::id()));
        let _ = std::fs::remove_file(&tmp);
        let _ = std::fs::remove_file(tmp.with_extension("tmp"));
        let mut mru = PickerMruIndex::new();
        mru.record("files", "x");
        mru.save_to(&tmp).expect("save");
        assert!(tmp.exists());
        assert!(!tmp.with_extension("tmp").exists());
        let _ = std::fs::remove_file(&tmp);
    }

    /// 14b: default_persist_path returns a path under the
    /// platform's cache dir (when one exists). Not asserting
    /// exact path because that varies per platform; just that
    /// the file name is right.
    #[test]
    fn default_persist_path_targets_picker_mru_file() {
        if let Some(path) = default_persist_path() {
            assert_eq!(
                path.file_name().and_then(|s| s.to_str()),
                Some("picker-mru.bincode")
            );
        }
    }
}
