//! Mode-driven completion sources (insert-completion.md §12).
//!
//! The trait surface every minor mode uses to contribute one or
//! more completion sources via `Mode::completion_sources()`. The
//! v1 architecture (§3 -- §11) hardcodes the source set inside
//! `lattice-ui-tui::app::completion::populate_insert_completion_sync`
//! and the bespoke `do_lsp_insert_completion_request` host code;
//! §12 of the design doc lays out the migration to a uniform
//! mode-contribution shape that:
//!
//! - lets WASM plugins register a completion source the same way
//!   they register any other mode contribution (options, keymap,
//!   subscriptions, decorations);
//! - relocates each source impl into the crate that owns its
//!   feature (LSP source -> `lattice-lsp`, snippet source ->
//!   `lattice-snippet`, etc.);
//! - keeps the per-keystroke hot path identical by caching the
//!   active source set per buffer in an `ActiveCompletionSources`
//!   buffer-local (§12.4).
//!
//! CSM.1 lands the type surface only -- nothing in the editor
//! references these types yet. CSM.2 -- CSM.8 wire production
//! code through them one source at a time.
//!
//! ## Two source shapes
//!
//! - [`SyncCompletionSource`]: cheap, blocking. The aggregator
//!   calls `produce()` directly on the popup-open / refilter
//!   path. Buffer-words, snippets, tree-sitter symbols, path
//!   completion all fit here -- microsecond-scale walks.
//! - [`AsyncCompletionSource`]: produces candidates via a future
//!   that pushes into a host-supplied [`CandidateSink`]. LSP
//!   (multi-server fan-out + isIncomplete refresh) is the
//!   driving case; plugin sources that round-trip to a backend
//!   use the same shape.
//!
//! The crate stays runtime-agnostic -- the async trait hands
//! back a `Pin<Box<dyn Future + Send>>` and the host (TUI today,
//! plugin host later) runs it on whatever executor it owns. No
//! tokio dependency in `lattice-completion`.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use lattice_protocol::CancellationToken;
use lattice_protocol::Position;

use crate::candidate::RawCandidate;
use crate::insert::{CompletionTrigger, InsertContext, SourceId};

/// One source's contribution to the active completion set for a
/// buffer. Returned from [`crate::Mode::completion_sources`] (the
/// new declarative contribution method on `Mode` -- see
/// `lattice-mode`). The aggregator's per-buffer cache holds a
/// list of these; the active-source resolver in the host
/// recomputes the cache only on mode-activation / -deactivation
/// transitions, so the keystroke-frequency refilter pays an
/// O(1) buffer-local lookup.
///
/// Cloning is cheap: every field is either `Copy` or an `Arc`.
#[derive(Clone)]
pub struct CompletionSourceContribution {
    /// Stable identifier (`"gen:lsp-completion"`,
    /// `"gen:buffer-words"`, ...). Surfaces in `:set
    /// completion.source.<id>.priority=...`, in `:describe-mode
    /// <name>` output, and in the per-language sources allowlist.
    pub id: SourceId,
    /// Default priority bucket. Higher buckets sort above lower;
    /// the host's per-buffer `priority_for_source` override may
    /// replace this value before the ranker reads it.
    pub default_priority: u32,
    /// Whether typing identifier chars opens the popup with this
    /// source included. Manual triggers (`<C-x><C-o>` /
    /// `<C-Space>`) always include every enabled source.
    pub auto_trigger: bool,
    /// Server-advertised or source-supplied characters that
    /// should fire this source. Empty = "fire on identifier-
    /// threshold or manual." The LSP source populates this from
    /// `completionProvider.triggerCharacters` at activation.
    pub trigger_chars: Vec<char>,
    /// CSM.K1 (insert-completion.md §12): single-char filter
    /// chord inside `completion-popup-mode`. `Some('o')` ⇒
    /// `<C-o>` while the popup is live narrows the rendered
    /// candidate set to *only* this source's contributions
    /// (mnemonic: o for omni → LSP). `None` ⇒ no dedicated
    /// chord; the source still participates in the unfiltered
    /// all-sources view + any TOML allowlist.
    ///
    /// CSM.K2 wires the binding -- `completion-popup-mode`'s
    /// keymap walks `ActiveCompletionSources` at push time and
    /// registers `<C-?>` for every contribution whose
    /// `popup_filter_chord` is `Some`. Until then this field
    /// is plumbing: CSM.4 -- CSM.8 fill it in for each migrated
    /// source so CSM.K2 can ship one slice's worth of behavior
    /// at a time.
    pub popup_filter_chord: Option<char>,
    /// The actual producer -- sync or async.
    pub kind: CompletionSourceKind,
}

impl fmt::Debug for CompletionSourceContribution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CompletionSourceContribution")
            .field("id", &self.id)
            .field("default_priority", &self.default_priority)
            .field("auto_trigger", &self.auto_trigger)
            .field("trigger_chars", &self.trigger_chars)
            .field("popup_filter_chord", &self.popup_filter_chord)
            .field("kind", &self.kind.kind_label())
            .finish()
    }
}

/// Discriminator for [`CompletionSourceContribution::kind`]. The
/// aggregator dispatches on this: `Sync` sources are called
/// inline during refilter; `Async` sources are spawned at popup-
/// open + on `isIncomplete` refresh, with the host running the
/// returned future on its executor.
#[derive(Clone)]
pub enum CompletionSourceKind {
    Sync(Arc<dyn SyncCompletionSource>),
    Async(Arc<dyn AsyncCompletionSource>),
}

impl CompletionSourceKind {
    /// Short human-readable tag for debug / `:describe-mode`
    /// output (`"sync"` or `"async"`).
    pub fn kind_label(&self) -> &'static str {
        match self {
            CompletionSourceKind::Sync(_) => "sync",
            CompletionSourceKind::Async(_) => "async",
        }
    }
}

/// Cheap, blocking completion source. Implementations walk the
/// buffer / a small registry / a cached symbol table and return
/// the matching `RawCandidate`s in a single call. The aggregator
/// invokes [`Self::produce`] once per refilter (once per popup-
/// query change). Sources strictly more expensive than ~100 us
/// per call should be [`AsyncCompletionSource`]s instead.
///
/// This is the post-§12 successor to the v1
/// [`crate::insert::InsertSource`] trait -- new sources should
/// target this trait; existing impls migrate per the slice
/// plan (CSM.4 -- CSM.7).
pub trait SyncCompletionSource: Send + Sync + fmt::Debug {
    /// Produce raw candidates for the supplied context. Cheap
    /// (microseconds); the aggregator calls this whenever the
    /// popup's query string changes.
    fn produce(&self, ctx: &InsertContext<'_>) -> Vec<RawCandidate>;
}

/// Asynchronous completion source. Used for sources that must
/// round-trip (LSP), watch a long-running task, or otherwise
/// can't deliver candidates synchronously. The aggregator calls
/// [`Self::produce_async`] at popup-open and on every
/// `isIncomplete` re-fire; the returned future pushes
/// candidates into a host-supplied [`CandidateSink`] as they
/// arrive.
///
/// The trait deliberately hands the host a generic `Future` --
/// `lattice-completion` does not depend on tokio. The host
/// (`lattice-ui-tui` today, the plugin runtime later) drives
/// the future on its own executor and is free to cancel via
/// the supplied [`CancellationToken`].
pub trait AsyncCompletionSource: Send + Sync + fmt::Debug {
    /// Build the producer future. The future:
    ///
    /// - reads the snapshot (which carries the cursor / query /
    ///   trigger; the source's own struct captures anything else
    ///   it needs -- handles, URIs, server lookups);
    /// - pushes each `RawCandidate` into `sink` as it arrives;
    /// - checks `token.is_cancelled()` at await points and bails
    ///   without further pushes when set;
    /// - resolves when the source has no more candidates to
    ///   produce (the aggregator marks the source "done" and
    ///   stops awaiting further work for this popup instance).
    ///
    /// `Arc<dyn CandidateSink>` is used (rather than borrowed
    /// `&dyn CandidateSink`) so the future can outlive the
    /// caller's stack frame -- it crosses the spawn boundary.
    fn produce_async(
        &self,
        ctx: InsertContextSnapshot,
        sink: Arc<dyn CandidateSink>,
        token: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// Host-supplied mailbox an [`AsyncCompletionSource`] pushes
/// candidates into. The host's `lattice-ui-tui` impl wraps a
/// `tokio::sync::mpsc::UnboundedSender<RawCandidate>` so the
/// per-frame drain merges async-source pushes into
/// `InsertCompletionState::raw`; a WASM-plugin host wraps
/// whatever its runtime provides.
///
/// `push` is fire-and-forget: a closed channel (popup
/// dismissed) silently drops the candidate. Sources should
/// rely on cancellation, not push success, to know when to
/// stop.
pub trait CandidateSink: Send + Sync {
    fn push(&self, candidate: RawCandidate);
}

/// Owned snapshot of `InsertContext` for crossing the
/// spawn / future boundary. The borrowed
/// [`crate::insert::InsertContext`] can't cross the spawn (it
/// holds `&Buffer` / `&str`); the snapshot copies the fields
/// the source actually needs.
///
/// CSM.1 ships the minimal snapshot (cursor / anchor / query /
/// trigger / case-sensitivity). Sources that need richer
/// context (e.g. tree-sitter walking the buffer's syntax tree)
/// will either:
///
/// - extend this struct with the relevant owned field
///   (a `Buffer` clone is `O(1)` thanks to ropey's
///   structural sharing);
/// - or capture the data they need through their own struct's
///   fields at construction (the LSP source captures
///   `LspSupervisorHandle`, the snippet source captures
///   `Arc<SnippetRegistry>`).
#[derive(Debug, Clone)]
pub struct InsertContextSnapshot {
    pub cursor: Position,
    pub anchor: Position,
    pub query: String,
    pub trigger: CompletionTrigger,
    pub case_sensitive: bool,
    /// Active buffer's language id (CSM.5). See
    /// [`InsertContext::language`].
    pub language: String,
    /// Pre-computed tree-sitter symbols (CSM.6). See
    /// [`InsertContext::tree_sitter_symbols`].
    pub tree_sitter_symbols: Vec<String>,
}

impl InsertContextSnapshot {
    /// Take an owned snapshot of `ctx`. Cheap-ish -- clones
    /// `query` + `language` + the `tree_sitter_symbols` slice;
    /// the other fields are `Copy`.
    pub fn from_context(ctx: &InsertContext<'_>) -> Self {
        Self {
            cursor: ctx.cursor,
            anchor: ctx.anchor,
            query: ctx.query.to_string(),
            trigger: ctx.trigger.clone(),
            case_sensitive: ctx.case_sensitive,
            language: ctx.language.to_string(),
            tree_sitter_symbols: ctx.tree_sitter_symbols.to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::candidate::CandidateKind;
    use lattice_core::Buffer;

    /// A minimal sync source for trait-shape tests. Production
    /// sources will live in their feature crates (CSM.4 -- CSM.7).
    #[derive(Debug)]
    struct EchoSync {
        id: SourceId,
        items: Vec<String>,
    }

    impl SyncCompletionSource for EchoSync {
        fn produce(&self, _ctx: &InsertContext<'_>) -> Vec<RawCandidate> {
            self.items
                .iter()
                .map(|s| RawCandidate::plain(s.clone(), CandidateKind::Plain))
                .collect()
        }
    }

    /// A trivial async source: pushes one fixed candidate then
    /// resolves. Real async sources (LSP) drive their work loop
    /// inside the future.
    #[derive(Debug)]
    struct EchoAsync {
        candidate: String,
    }

    impl AsyncCompletionSource for EchoAsync {
        fn produce_async(
            &self,
            _ctx: InsertContextSnapshot,
            sink: Arc<dyn CandidateSink>,
            _token: CancellationToken,
        ) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            let text = self.candidate.clone();
            Box::pin(async move {
                sink.push(RawCandidate::plain(text, CandidateKind::Plain));
            })
        }
    }

    #[test]
    fn sync_source_produces_candidates() {
        let buffer = Buffer::empty();
        let ctx = InsertContext {
            buffer: &buffer,
            cursor: Position::ZERO,
            anchor: Position::ZERO,
            query: "",
            trigger: &CompletionTrigger::Manual,
            case_sensitive: false,
            language: "",
            tree_sitter_symbols: &[],
        };
        let src = EchoSync {
            id: SourceId::new("gen:echo"),
            items: vec!["alpha".into(), "beta".into()],
        };
        let candidates = src.produce(&ctx);
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].text, "alpha");
        let _ = src.id;
    }

    #[test]
    fn contribution_is_constructible_and_debug_readable() {
        let contribution = CompletionSourceContribution {
            id: SourceId::new("gen:echo"),
            default_priority: 100,
            auto_trigger: true,
            trigger_chars: vec!['.'],
            popup_filter_chord: Some('e'),
            kind: CompletionSourceKind::Sync(Arc::new(EchoSync {
                id: SourceId::new("gen:echo"),
                items: vec!["x".into()],
            })),
        };
        assert_eq!(contribution.id.as_str(), "gen:echo");
        assert_eq!(contribution.kind.kind_label(), "sync");
        // Debug doesn't panic + names the relevant fields.
        let dbg = format!("{contribution:?}");
        assert!(dbg.contains("gen:echo"));
        assert!(dbg.contains("sync"));
    }

    #[test]
    fn async_source_pushes_observable_via_shared_handle() {
        // Lightweight executor for the test: poll the future once
        // with a no-op waker. `EchoAsync` doesn't `.await`, so one
        // poll completes it. Keeps the crate free of an executor
        // dep; real async drive happens host-side on tokio.
        use std::sync::Mutex;
        use std::task::{Context, Poll, Wake};
        struct SharedSink {
            received: Arc<Mutex<Vec<String>>>,
        }
        impl CandidateSink for SharedSink {
            fn push(&self, candidate: RawCandidate) {
                self.received.lock().unwrap().push(candidate.text);
            }
        }
        struct Noop;
        impl Wake for Noop {
            fn wake(self: Arc<Self>) {}
        }
        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink: Arc<dyn CandidateSink> = Arc::new(SharedSink {
            received: received.clone(),
        });
        let src = EchoAsync {
            candidate: "lsp-pushed-item".into(),
        };
        let buffer = Buffer::empty();
        let ctx = InsertContext {
            buffer: &buffer,
            cursor: Position::ZERO,
            anchor: Position::ZERO,
            query: "",
            trigger: &CompletionTrigger::Manual,
            case_sensitive: false,
            language: "",
            tree_sitter_symbols: &[],
        };
        let snap = InsertContextSnapshot::from_context(&ctx);
        let mut fut = src.produce_async(snap, sink, CancellationToken::never());
        let waker = Arc::new(Noop).into();
        let mut cx = Context::from_waker(&waker);
        assert!(matches!(fut.as_mut().poll(&mut cx), Poll::Ready(())));
        let pushed = received.lock().unwrap();
        assert_eq!(pushed.as_slice(), &["lsp-pushed-item".to_string()]);
    }

    #[test]
    fn snapshot_clones_query_owns_the_rest() {
        let buffer = Buffer::empty();
        let ctx = InsertContext {
            buffer: &buffer,
            cursor: Position::new(2, 7),
            anchor: Position::new(2, 4),
            query: "foo",
            trigger: &CompletionTrigger::IdentifierThreshold,
            case_sensitive: true,
            language: "rust",
            tree_sitter_symbols: &[],
        };
        let snap = InsertContextSnapshot::from_context(&ctx);
        assert_eq!(snap.cursor, Position::new(2, 7));
        assert_eq!(snap.anchor, Position::new(2, 4));
        assert_eq!(snap.query, "foo");
        assert_eq!(snap.case_sensitive, true);
        assert_eq!(snap.language, "rust");
        assert!(matches!(
            snap.trigger,
            CompletionTrigger::IdentifierThreshold
        ));
    }
}
