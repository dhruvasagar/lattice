//! OM.A1 — the registry of agenda-row producers.
//!
//! The agenda twin of [`media_source`](crate::media_source), and the same
//! contract: an async, off-keystroke-path producer the host drives on a
//! trigger. Where a media source answers "what images does this buffer show",
//! an agenda source answers "what dated rows does this FILE contribute" —
//! once per file of a project walk.
//!
//! Nothing here names org. A source declares the extensions it wants offered
//! ([`AsyncAgendaSource::extensions`]) and the host offers it only those
//! files, which is what keeps `.org` out of the host walk.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use arc_swap::ArcSwap;

/// One agenda row a producer found in a file.
///
/// The native mirror of the WIT `entry` (`wit/agenda-source.wit`). It is a
/// *span in a file*, not a rendered string, because the agenda is literally a
/// multibuffer of excerpts — which is what buys jump-to-source and
/// edit-propagates-to-source for free (`org-mode.md` §6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgendaEntry {
    /// 0-based first line of the row's excerpt.
    pub line: u32,
    /// 0-based last line of the excerpt, inclusive. Equal to `line` for the
    /// one-row-per-headline case.
    pub end_line: u32,
    /// Grouping **key**. Rows that sort adjacently and share a key render
    /// under one header. A key rather than a label because a producer
    /// cannot know which of its rows lands first once other files' rows
    /// are interleaved — the host compares keys after the sort.
    pub group: String,
    /// The header title, used when this row turns out to start a group.
    pub label: String,
    /// The host stable-sorts every file's rows together on this, ascending.
    /// The producer owns what it means.
    pub sort_key: i64,
}

/// The boxed future an [`AsyncAgendaSource::scan`] returns.
///
/// `Err(reason)` skips THIS FILE and the scan continues — one malformed file
/// must not fail the agenda. That is `error-parser`'s rule, because it is the
/// same failure class.
pub type AgendaFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<AgendaEntry>, String>> + Send + 'a>>;

/// The boxed future an [`AsyncAgendaSource::begin`] returns.
///
/// Separate from [`AgendaFuture`] rather than reusing it with an ignored
/// `Vec`: `begin` produces nothing, and a signature that says otherwise
/// invites a producer to return rows from it that the scan would drop.
pub type AgendaBeginFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// An async, off-keystroke-path producer of agenda rows.
pub trait AsyncAgendaSource: Send + Sync + std::fmt::Debug {
    /// Stable id of the producing plugin — the teardown key. Two producers
    /// with the same id are the same plugin, so a reload replaces rather than
    /// duplicates.
    fn source_id(&self) -> u64;

    /// File extensions this source wants offered, lowercased and without the
    /// leading dot. Resolved once at registration and cached here, so the
    /// walk's per-file test is a string compare rather than a guest call.
    fn extensions(&self) -> &[String];

    /// Drop per-scan state. Called once before the first file of a scan.
    ///
    /// An `Err` drops this source from the scan (its state is unknown, so its
    /// rows would be untrustworthy) while every other source carries on.
    fn begin(&self) -> AgendaBeginFuture<'_>;

    /// Scan one file. `text` is the file's contents, already read by the host
    /// — the host must read it anyway to build the source `Document`, so it
    /// reads once and hands the text over.
    fn scan(&self, path: PathBuf, text: String) -> AgendaFuture<'_>;

    /// True when `path`'s extension is one this source claimed.
    fn claims(&self, path: &std::path::Path) -> bool {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        let lowered = ext.to_ascii_lowercase();
        self.extensions().iter().any(|e| *e == lowered)
    }
}

/// Runtime-mutable registry of [`AsyncAgendaSource`]s.
#[derive(Default, Clone)]
pub struct AgendaSourceRegistry {
    sources: Vec<Arc<dyn AsyncAgendaSource>>,
}

impl std::fmt::Debug for AgendaSourceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgendaSourceRegistry")
            .field("sources", &self.sources.len())
            .finish()
    }
}

impl AgendaSourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a producer. Idempotent per `source_id`: a re-register
    /// (reload) replaces rather than accumulating a duplicate — otherwise
    /// every `:plugin-reload` would double every row in the agenda.
    pub fn register(&mut self, source: Arc<dyn AsyncAgendaSource>) {
        let id = source.source_id();
        self.sources.retain(|s| s.source_id() != id);
        self.sources.push(source);
    }

    /// Unregister every producer for `source_id`; returns the count removed.
    /// No-op when absent, per the teardown contract.
    pub fn unregister(&mut self, source_id: u64) -> usize {
        let before = self.sources.len();
        self.sources.retain(|s| s.source_id() != source_id);
        before - self.sources.len()
    }

    /// A snapshot of the registered producers.
    pub fn sources(&self) -> Vec<Arc<dyn AsyncAgendaSource>> {
        self.sources.clone()
    }

    /// Every source claiming `path`'s extension.
    ///
    /// The walk asks this per file. Returning the matching sources rather
    /// than a bool means a file claimed by two producers is offered to both,
    /// which is the honest answer when a markdown TODO scanner and a
    /// checklist scanner both want `.md`.
    pub fn claiming(&self, path: &std::path::Path) -> Vec<Arc<dyn AsyncAgendaSource>> {
        self.sources
            .iter()
            .filter(|s| s.claims(path))
            .cloned()
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    pub fn len(&self) -> usize {
        self.sources.len()
    }
}

/// Boot-service handle. Register **and** look up with this exact alias (the
/// `ServiceRegistry` TypeId rule).
pub type AgendaSourceRegistryHandle = Arc<ArcSwap<AgendaSourceRegistry>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[derive(Debug)]
    struct Fake {
        id: u64,
        exts: Vec<String>,
    }

    impl Fake {
        fn new(id: u64, exts: &[&str]) -> Self {
            Self {
                id,
                exts: exts.iter().map(|e| e.to_string()).collect(),
            }
        }
    }

    impl AsyncAgendaSource for Fake {
        fn source_id(&self) -> u64 {
            self.id
        }
        fn extensions(&self) -> &[String] {
            &self.exts
        }
        fn begin(&self) -> AgendaBeginFuture<'_> {
            Box::pin(async { Ok(()) })
        }
        fn scan(&self, _p: PathBuf, _t: String) -> AgendaFuture<'_> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    /// A reload must REPLACE its producer, not add a second one — otherwise
    /// every `:plugin-reload` doubles every row in the agenda.
    #[test]
    fn re_registering_the_same_source_id_replaces_rather_than_duplicates() {
        let mut r = AgendaSourceRegistry::new();
        r.register(Arc::new(Fake::new(7, &["org"])));
        r.register(Arc::new(Fake::new(7, &["org"])));
        assert_eq!(r.len(), 1);
        r.register(Arc::new(Fake::new(8, &["md"])));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn unregister_reports_what_it_removed_and_is_idempotent() {
        let mut r = AgendaSourceRegistry::new();
        r.register(Arc::new(Fake::new(7, &["org"])));
        assert_eq!(r.unregister(7), 1);
        assert_eq!(r.unregister(7), 0, "idempotent, per the teardown contract");
        assert!(r.is_empty());
    }

    /// The property that keeps `.org` out of the host walk: which files get
    /// offered is the SOURCE's answer, not the host's.
    #[test]
    fn only_sources_claiming_the_extension_are_offered_a_file() {
        let mut r = AgendaSourceRegistry::new();
        r.register(Arc::new(Fake::new(1, &["org"])));
        r.register(Arc::new(Fake::new(2, &["md", "markdown"])));

        let org = r.claiming(Path::new("/p/notes.org"));
        assert_eq!(org.len(), 1);
        assert_eq!(org[0].source_id(), 1);

        assert_eq!(r.claiming(Path::new("/p/README.md")).len(), 1);
        assert_eq!(r.claiming(Path::new("/p/main.rs")).len(), 0);
    }

    /// A file two producers both claim is offered to both — the honest
    /// answer when a TODO scanner and a checklist scanner both want `.md`.
    #[test]
    fn a_file_claimed_by_two_sources_is_offered_to_both() {
        let mut r = AgendaSourceRegistry::new();
        r.register(Arc::new(Fake::new(1, &["md"])));
        r.register(Arc::new(Fake::new(2, &["md"])));
        assert_eq!(r.claiming(Path::new("/p/x.md")).len(), 2);
    }

    /// `.ORG` is the same filetype as `.org`. The host lowercases the
    /// extension; the loader lowercases what the guest declared.
    #[test]
    fn extension_matching_is_case_insensitive() {
        let f = Fake::new(1, &["org"]);
        assert!(f.claims(Path::new("/p/NOTES.ORG")));
        assert!(f.claims(Path::new("/p/notes.org")));
    }

    /// An extensionless file (`Makefile`, a dotfile) matches nothing rather
    /// than matching everything.
    #[test]
    fn a_file_with_no_extension_is_claimed_by_nobody() {
        let f = Fake::new(1, &["org"]);
        assert!(!f.claims(Path::new("/p/Makefile")));
        assert!(!f.claims(Path::new("/p/.gitignore")));
    }
}
