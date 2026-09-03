//! OM.A1 — the registry of agenda-row producers.
//!
//! The agenda twin of [`media_source`](crate::media_source), and the same
//! contract: an async, off-keystroke-path producer the host drives on a
//! trigger. Where a media source answers "what images does this buffer show",
//! an agenda source answers "what dated rows does this FILE contribute" —
//! once per file of a project walk.
//!
//! Nothing here names org. A source declares the extensions it wants offered
//! ([`ScannedExcerptSource::extensions`]) and the host offers it only those
//! files, which is what keeps `.org` out of the host walk.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use arc_swap::ArcSwap;

/// One agenda row a producer found in a file.
///
/// The native mirror of the WIT `entry` (`wit/scanned-excerpt-source.wit`). It is a
/// *span in a file*, not a rendered string, because the agenda is literally a
/// multibuffer of excerpts — which is what buys jump-to-source and
/// edit-propagates-to-source for free (`org-mode.md` §6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScannedExcerpt {
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
    /// OA.5: how this row is coloured, as byte spans into the row's own first
    /// line — NOT into the composed view, which the producer cannot see until
    /// every other file's rows have been interleaved by the sort.
    ///
    /// Empty is the ordinary case, and means "say nothing about colour": the
    /// source file's own grammar highlighting is what shows, unchanged. A
    /// producer fills this when the row has semantics its grammar does not
    /// carry — an agenda's TODO keyword, priority and tags are org's, not the
    /// org grammar's, which is why an agenda looked like org text out of
    /// order before this existed.
    pub spans: Vec<RowSpan>,
    /// HB.5: a row to hang BELOW this one, or `None`.
    ///
    /// The WIT `annotation`, native side. A row's text is a verbatim excerpt of
    /// a source line, so a producer with something of its own to show — a
    /// habit's consistency graph — has nowhere to put it; this becomes a
    /// virtual row anchored below instead.
    ///
    /// `None` is the ordinary case. A scan of plain TODOs grows no second rows.
    pub annotation: Option<RowAnnotation>,
}

/// HB.5: one line hung below a row, and how it is coloured.
///
/// The WIT `annotation`, native side. Its [`spans`](Self::spans) index into
/// [`text`](Self::text) — not into the row's source line, which this is not
/// part of — and resolve through the same path [`RowSpan`] does.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowAnnotation {
    pub text: String,
    pub spans: Vec<RowSpan>,
}

/// One styled run within a row, naming a style rather than carrying one.
///
/// The WIT `display-span`, native side. `slot` resolves host-side through the
/// same path a `highlights.scm` capture takes, so a plugin's own registered
/// theme element (`org.todo.WAITING`) reaches the row with the active
/// colourscheme applied, and an unresolvable name renders unstyled rather
/// than failing the row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSpan {
    /// Byte offset from the start of the row's line.
    pub start: u32,
    pub end: u32,
    /// Capture or theme-element name.
    pub slot: String,
}

/// OA.14b: time clocked on one headline on one day.
///
/// The WIT `clock-span`, native side. Reported for every clocked headline a
/// producer saw — NOT only for the ones that became rows. A clock report totals
/// what you actually logged, and agenda rows are a filtered subset, so a
/// headline clocked yesterday with no TODO and no date must still count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClockSpan {
    /// 0-based line of the HEADLINE the time was logged under.
    pub line: u32,
    /// Outline path, outermost ancestor first, the headline itself last. Its
    /// length is the outline level.
    ///
    /// A path rather than a name plus a level, because the report is a
    /// hierarchy whose totals roll up it: an ancestor that logged no time of
    /// its own emits no span, so the chain is the only way to name it.
    pub outline: Vec<String>,
    /// Days since the Unix epoch the time is filed under.
    pub day: i64,
    /// Minutes clocked, already summed per (headline, day) by the producer.
    pub minutes: u32,
}

/// What one file's scan produced.
///
/// A record rather than a bare row list because the clock report is not a view
/// of the rows — see [`ClockSpan`]. It rides the same call so the walk still
/// makes ONE producer call per file: the scan is a producer's critical path,
/// and a second crossing to carry data most files have none of would double it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanResult {
    pub entries: Vec<ScannedExcerpt>,
    pub clock: Vec<ClockSpan>,
}

impl ScanResult {
    /// The common case: rows and nothing clocked.
    pub fn rows(entries: Vec<ScannedExcerpt>) -> Self {
        Self {
            entries,
            clock: Vec::new(),
        }
    }
}

/// The boxed future an [`ScannedExcerptSource::scan`] returns.
///
/// `Err(reason)` skips THIS FILE and the scan continues — one malformed file
/// must not fail the agenda. That is `error-parser`'s rule, because it is the
/// same failure class.
pub type ScanFuture<'a> = Pin<Box<dyn Future<Output = Result<ScanResult, String>> + Send + 'a>>;

/// The boxed future an [`ScannedExcerptSource::begin`] returns.
///
/// Separate from [`ScanFuture`] rather than reusing it with an ignored
/// `Vec`: `begin` produces nothing, and a signature that says otherwise
/// invites a producer to return rows from it that the scan would drop.
pub type ScanBeginFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// OA.22: the future [`ScannedExcerptSource::describe`] returns.
///
/// Infallible by design. A source that cannot say what it is has nothing to
/// report rather than an error to raise, and the header falls back to the plain
/// form — failing a whole scan because its label did not render would be the
/// tail wagging the dog.
pub type ScanDescribeFuture<'a> = Pin<Box<dyn Future<Output = String> + Send + 'a>>;

/// The boxed future an [`ScannedExcerptSource::roots`] returns (AF.1).
pub type ScanRootsFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<String>, String>> + Send + 'a>>;

/// An async, off-keystroke-path producer of agenda rows.
pub trait ScannedExcerptSource: Send + Sync + std::fmt::Debug {
    /// Stable id of the producing plugin — the teardown key. Two producers
    /// with the same id are the same plugin, so a reload replaces rather than
    /// duplicates.
    fn source_id(&self) -> u64;

    /// File extensions this source wants offered, lowercased and without the
    /// leading dot. Resolved once at registration and cached here, so the
    /// walk's per-file test is a string compare rather than a guest call.
    fn extensions(&self) -> &[String];

    /// A minor mode this source wants activated on the agenda view.
    ///
    /// How a source acts on its own rows. The view's generic behaviour —
    /// jump-to-source, `gr` — is the host's, because the host built the view
    /// and is the only thing that can re-walk it; but the *semantics* of a
    /// row belong to whoever produced it, and those need chords in a buffer
    /// whose major is `multibuffer-mode`. No activation policy can say "the
    /// buffer this provider just built", so the provider activates it and
    /// this is the source naming what.
    fn view_mode(&self) -> Option<&str> {
        None
    }

    /// AF.1: the paths this source wants scanned — each a FILE or a DIRECTORY.
    ///
    /// Empty means "no opinion": the host uses the root it would have used, so
    /// a source that does not implement this behaves exactly as before. That is
    /// why it has a default and `extensions` does not — a source with no
    /// extensions scans nothing and is a bug worth surfacing, while a source
    /// with no roots is the ordinary unconfigured case.
    ///
    /// Called PER SCAN, unlike `extensions` and `view_mode`, which are facts
    /// about the source and are cached at load. This answer comes from user
    /// configuration and must follow a `:set` without a reload.
    ///
    /// An `Err` is logged and treated as empty: a source that cannot say where
    /// to look should not be able to make the agenda scan nothing.
    fn roots(&self) -> ScanRootsFuture<'_> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Drop per-scan state. Called once before the first file of a scan.
    ///
    /// An `Err` drops this source from the scan (its state is unknown, so its
    /// rows would be untrustworthy) while every other source carries on.
    ///
    /// OA.11a: `args` is what the VIEW was opened with, passed through
    /// **uninterpreted**. The host routes these; it does not read them. They
    /// are how one source serves more than one scan — org's agenda dispatcher
    /// names which custom command to run — and they are deliberately not the
    /// provider view's `argument`, which is the root override and *is*
    /// host-interpreted because the host does the walk.
    ///
    /// Called before [`Self::roots`], so a source that stashes its args here
    /// has them for `roots`, every `scan`, and the generation key it returns.
    /// Empty is the ordinary case: the default scan.
    fn begin(&self, args: &[String]) -> ScanBeginFuture<'_>;

    /// OA.22: what this view IS, in the source's own words, for its headerline.
    ///
    /// The host knows only how many rows it composed and how many files it
    /// walked; it deliberately does not read `args` (see [`Self::begin`]). So an
    /// agenda narrowed to one tag looks exactly like an unfiltered one — and
    /// "you have no tasks" is the worst thing this view can say incorrectly.
    ///
    /// A short phrase naming the command, the span and any active filters. The
    /// caller prefixes its own counts, so this must not repeat them. Empty
    /// means "nothing worth saying" and the header keeps its plain form, which
    /// is why the default is exactly that: a source with no view state to
    /// report implements nothing.
    ///
    /// Called ONCE per scan, after `begin` — off the per-file path.
    fn describe(&self, _args: &[String]) -> ScanDescribeFuture<'_> {
        Box::pin(async { String::new() })
    }

    /// Scan one file. `text` is the file's contents, already read by the host
    /// — the host must read it anyway to build the source `Document`, so it
    /// reads once and hands the text over.
    fn scan(&self, path: PathBuf, text: String) -> ScanFuture<'_>;

    /// True when `path`'s extension is one this source claimed.
    fn claims(&self, path: &std::path::Path) -> bool {
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        let lowered = ext.to_ascii_lowercase();
        self.extensions().iter().any(|e| *e == lowered)
    }
}

/// Runtime-mutable registry of [`ScannedExcerptSource`]s.
#[derive(Default, Clone)]
pub struct ScannedExcerptSourceRegistry {
    sources: Vec<Arc<dyn ScannedExcerptSource>>,
}

impl std::fmt::Debug for ScannedExcerptSourceRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScannedExcerptSourceRegistry")
            .field("sources", &self.sources.len())
            .finish()
    }
}

impl ScannedExcerptSourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a producer. Idempotent per `source_id`: a re-register
    /// (reload) replaces rather than accumulating a duplicate — otherwise
    /// every `:plugin-reload` would double every row in the agenda.
    pub fn register(&mut self, source: Arc<dyn ScannedExcerptSource>) {
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
    pub fn sources(&self) -> Vec<Arc<dyn ScannedExcerptSource>> {
        self.sources.clone()
    }

    /// Every minor mode a registered source wants on the agenda view.
    ///
    /// Deduplicated, and returned for EVERY registered source rather than
    /// only the ones that contributed rows: a source's chords must be present
    /// before the scan finishes, and a mode whose actions no-op off its own
    /// rows is harmless where a mode that arrives late is a key that works on
    /// the second try.
    pub fn view_modes(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for s in &self.sources {
            if let Some(m) = s.view_mode()
                && !out.iter().any(|e| e == m)
            {
                out.push(m.to_string());
            }
        }
        out
    }

    /// Every source claiming `path`'s extension.
    ///
    /// The walk asks this per file. Returning the matching sources rather
    /// than a bool means a file claimed by two producers is offered to both,
    /// which is the honest answer when a markdown TODO scanner and a
    /// checklist scanner both want `.md`.
    pub fn claiming(&self, path: &std::path::Path) -> Vec<Arc<dyn ScannedExcerptSource>> {
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
pub type ScannedExcerptSourceRegistryHandle = Arc<ArcSwap<ScannedExcerptSourceRegistry>>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[derive(Debug)]
    struct Fake {
        id: u64,
        exts: Vec<String>,
        view_mode: Option<String>,
    }

    impl Fake {
        fn new(id: u64, exts: &[&str]) -> Self {
            Self {
                id,
                exts: exts.iter().map(|e| e.to_string()).collect(),
                view_mode: None,
            }
        }

        fn with_view_mode(mut self, m: &str) -> Self {
            self.view_mode = Some(m.to_string());
            self
        }
    }

    impl ScannedExcerptSource for Fake {
        fn source_id(&self) -> u64 {
            self.id
        }
        fn extensions(&self) -> &[String] {
            &self.exts
        }
        fn view_mode(&self) -> Option<&str> {
            self.view_mode.as_deref()
        }
        fn begin(&self, _args: &[String]) -> ScanBeginFuture<'_> {
            Box::pin(async { Ok(()) })
        }
        fn scan(&self, _p: PathBuf, _t: String) -> ScanFuture<'_> {
            Box::pin(async { Ok(ScanResult::default()) })
        }
    }

    /// A reload must REPLACE its producer, not add a second one — otherwise
    /// every `:plugin-reload` doubles every row in the agenda.
    #[test]
    fn re_registering_the_same_source_id_replaces_rather_than_duplicates() {
        let mut r = ScannedExcerptSourceRegistry::new();
        r.register(Arc::new(Fake::new(7, &["org"])));
        r.register(Arc::new(Fake::new(7, &["org"])));
        assert_eq!(r.len(), 1);
        r.register(Arc::new(Fake::new(8, &["md"])));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn unregister_reports_what_it_removed_and_is_idempotent() {
        let mut r = ScannedExcerptSourceRegistry::new();
        r.register(Arc::new(Fake::new(7, &["org"])));
        assert_eq!(r.unregister(7), 1);
        assert_eq!(r.unregister(7), 0, "idempotent, per the teardown contract");
        assert!(r.is_empty());
    }

    /// The property that keeps `.org` out of the host walk: which files get
    /// offered is the SOURCE's answer, not the host's.
    #[test]
    fn only_sources_claiming_the_extension_are_offered_a_file() {
        let mut r = ScannedExcerptSourceRegistry::new();
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
        let mut r = ScannedExcerptSourceRegistry::new();
        r.register(Arc::new(Fake::new(1, &["md"])));
        r.register(Arc::new(Fake::new(2, &["md"])));
        assert_eq!(r.claiming(Path::new("/p/x.md")).len(), 2);
    }

    /// Every source's mode is offered, deduplicated — two org-shaped plugins
    /// naming one mode must not activate it twice, and a source with no mode
    /// must not contribute an entry.
    #[test]
    fn view_modes_are_collected_and_deduplicated() {
        let mut r = ScannedExcerptSourceRegistry::new();
        r.register(Arc::new(
            Fake::new(1, &["org"]).with_view_mode("org-agenda-mode"),
        ));
        r.register(Arc::new(Fake::new(2, &["md"])));
        r.register(Arc::new(
            Fake::new(3, &["txt"]).with_view_mode("org-agenda-mode"),
        ));
        assert_eq!(r.view_modes(), vec!["org-agenda-mode".to_string()]);
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
