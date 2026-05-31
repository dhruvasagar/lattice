//! D.3.a (2026-05-29) — `DiffOverlayVirtualRowProvider`.
//!
//! Bridges [`crate::diff::subsystem::DiffSession`]'s published
//! `HunkIndex` to the `virtual_rows_worker`'s
//! [`lattice_cells::VirtualRowProvider`] surface. One provider
//! per active diff session; registered with
//! `Editor::virtual_row_providers` at session-open time
//! (D.3.a.1's `:diff` ex-command) and unregistered at
//! `:diffoff`.
//!
//! ## What this slice (D.3.a) emits
//!
//! For each `Remove` or `Change` hunk in the session's
//! published `HunkIndex`, one `Above`-anchored [`VirtualRow`]
//! per *baseline line* the hunk deletes / replaces. The
//! anchor is the **current side's start line** for the hunk —
//! so the deletion block appears immediately above the
//! corresponding edit position in the buffer the user is
//! looking at. `Add` hunks emit nothing (the added lines are
//! visible in the current buffer; the future gutter sign /
//! background tint passes — D.3.d / D.3.e — visualise the
//! add). `Conflict` hunks are emitted like `Change` for v1;
//! the three-way merge slice (D.6) refines the rendering.
//!
//! ## What this slice does NOT emit
//!
//! - **No cell content** in the deletion-block rows. D.3.a
//!   emits empty `cells: Arc<[Cell]>` so the virtual rows take
//!   visual space (height = 1 each) but render as blank
//!   placeholders. **D.3.b** lands the baseline-line text:
//!   the provider snapshots the descriptor's baseline rope
//!   on each revision bump, caches the rendered cells, and
//!   serves them from `collect()`.
//! - **No gutter signs** — D.3.d.
//! - **No background tints** — D.3.e.
//!
//! ## Why empty rows are still useful in D.3.a
//!
//! The user can immediately see *that* a hunk exists at the
//! right place — there's a visual gap above the current line
//! indicating "something was deleted / changed here." The
//! semantic plumbing (provider registration, revision
//! tracking, wake propagation, worker recompute on hunk
//! publish) all lights up. D.3.b adds the textual content
//! over the same wiring.
//!
//! ## Versioning
//!
//! `version()` returns the session's currently-published
//! `HunkIndex::revision`. Bumps on every successful publish
//! through [`crate::diff::subsystem::DiffSession::try_publish_if_newer`];
//! the `virtual_rows_worker`'s fingerprint pass picks up the
//! change on its next wake and triggers a recompute.
//! D.3.a.1's `:diff` ex-command also wires a wake forwarder
//! so a hunk publish fires the worker's `VirtualRowsWake`
//! directly — without it, the worker would only notice on the
//! next `publish_render_state` tick.

use std::sync::{Arc, Mutex};

use lattice_cells::{AnchorPosition, Cell, ProviderId, VirtualRow, VirtualRowProvider};
use lattice_diff::{HunkIndex, HunkKind};

use lattice_core::BufferId;
use tokio::task::JoinHandle;
use tracing::debug;

use crate::diff::subsystem::{BaselineSource, DiffSession};

// ──────────────────────────────────────────────────────────────
// D.3.d.0 (2026-05-29): per-line sign classification.
// ──────────────────────────────────────────────────────────────

/// Per-line gutter sign kind. Renderer-facing surface for
/// D.3.d.1 (TUI) and D.3.d.2 (GPUI sprite atlas) integrations
/// — D.3.d.0 lands the data layer only. D.3.e (line tints)
/// composes on top of the same classification.
///
/// `Add` and `Remove` are emitted for the obvious cases.
/// `Change` is emitted on **both** the baseline-deleted lines
/// (covered by the deletion-block virtual rows) and the
/// current-side replaced lines (which sit in the actual
/// document rows). A row carrying a `Change` sign tells the
/// renderer "this line replaces baseline content" — useful
/// for the tint pass.
///
/// `Conflict` (D.6.f, 2026-05-31) classifies a current-side
/// row that sits inside a three-way merge Conflict hunk —
/// both `local` and `remote` mutated the same `base` region
/// differently. Renders with a distinct glyph (`?`) and tint
/// (`theme.diff_conflict_line_bg`). The variant only fires
/// for three-way sessions; two-way `compute_two_way`
/// doesn't emit `HunkKind::Conflict`, so two-way overlays
/// never produce `DiffSignKind::Conflict`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DiffSignKind {
	Add,
	Remove,
	Change,
	Conflict,
}

/// Sparse per-line classification of the current-side rope.
///
/// Keyed by source line index (0-based, into the current
/// rope — i.e., the line the user is looking at). Sparse
/// because most lines have no sign. `entries` is sorted by
/// line so renderer-side lookup (per-row) is `O(log n)` via
/// binary search.
///
/// Build via [`compute_diff_sign_map`] — pure function of an
/// `Arc<HunkIndex>`. Published per-session via
/// `DiffSession::sign_map_cell` (D.3.d.0); the
/// [`DiffOverlayRefreshTask`] refreshes it on every hunk
/// publish so renderers see consistent decorations.
#[derive(Clone, Debug, Default)]
pub struct DiffSignMap {
	entries: Vec<(u32, DiffSignKind)>,
	/// The session revision the map was computed against.
	/// Renderers can compare against the session's
	/// `current_hunks().revision` to detect staleness.
	revision: u64,
}

impl DiffSignMap {
	pub fn entries(&self) -> &[(u32, DiffSignKind)] {
		&self.entries
	}

	pub fn revision(&self) -> u64 {
		self.revision
	}

	pub fn is_empty(&self) -> bool {
		self.entries.is_empty()
	}

	pub fn len(&self) -> usize {
		self.entries.len()
	}

	/// Lookup the sign for `line`, if any. `O(log n)` binary
	/// search over `entries`. Renderer hot path.
	pub fn sign_at(&self, line: u32) -> Option<DiffSignKind> {
		match self.entries.binary_search_by_key(&line, |(l, _)| *l) {
			Ok(idx) => Some(self.entries[idx].1),
			Err(_) => None,
		}
	}
}

/// D.3.d.0: derive a `DiffSignMap` from a `HunkIndex`.
///
/// Walks each hunk's current-side range (`ranges[1]`):
/// - `Add` → every line in the range gets `Add`.
/// - `Change` / `Conflict` → every line in the range gets
///   `Change` (the current-side rows are the replacements).
/// - `Remove` → no current-side lines exist; the deletion is
///   surfaced through the virtual-row deletion block (D.3.b)
///   and gets no sign in the sign map. Renderers wanting to
///   sign the *insertion point* of a removed hunk should
///   render based on the deletion block's anchor line (a
///   future D.3.d.1 detail) — keeping the sign map a
///   strictly-current-rope decoration here avoids
///   double-counting at the deletion anchor.
///
/// Entries are sorted by line before return so
/// `DiffSignMap::sign_at` binary-searches cheaply.
pub fn compute_diff_sign_map(hunks: &HunkIndex) -> DiffSignMap {
	let mut entries: Vec<(u32, DiffSignKind)> = Vec::new();
	for hunk in &hunks.hunks {
		let Some(current_range) = hunk.ranges.get(1) else {
			continue;
		};
		let kind = match hunk.kind {
			HunkKind::Add => DiffSignKind::Add,
			HunkKind::Change => DiffSignKind::Change,
			// D.6.f (2026-05-31): three-way Conflict gets its
			// own classification so renderers can decorate it
			// distinctly from a plain Change. Two-way sessions
			// never see Conflict hunks (`compute_two_way`
			// doesn't emit them) so this arm only fires for
			// three-way overlays.
			HunkKind::Conflict => DiffSignKind::Conflict,
			HunkKind::Remove => continue,
		};
		for line in current_range.start..current_range.end {
			entries.push((line, kind));
		}
	}
	entries.sort_by_key(|(l, _)| *l);
	DiffSignMap {
		entries,
		revision: hunks.revision,
	}
}

/// D.3.a.1 (2026-05-29): the [`ProviderId`] this slice uses
/// for a given session's overlay provider. Exposed as a free
/// function so `:diffoff` can unregister without holding the
/// session — the namespace prefix + buffer-id encoding makes
/// the id deterministic.
pub fn diff_overlay_provider_id(buffer_id: BufferId) -> ProviderId {
	DIFF_OVERLAY_PROVIDER_NAMESPACE | u64::from(buffer_id.0)
}

/// Namespace prefix for diff-overlay [`ProviderId`]s. We
/// OR-mix the session's `BufferId` into the low 32 bits;
/// uniqueness across the `virtual_row_providers` registry
/// holds as long as no other provider kind uses the same
/// namespace prefix. Documented here so the constant is
/// auditable.
const DIFF_OVERLAY_PROVIDER_NAMESPACE: u64 = 0xD1FF_0000_0000_0000;

/// Cached rendered virtual rows. Refreshed off the worker
/// thread by [`DiffOverlayRefreshTask`] (D.3.b) so
/// [`DiffOverlayVirtualRowProvider::collect`] returns without
/// blocking the virtual-rows worker.
///
/// `pub` so [`DiffOverlayRefreshTask::spawn`] and
/// [`DiffOverlayVirtualRowProvider::cache_handle`] can carry
/// the type through their signatures. Construction is internal
/// (use `Default::default`).
#[derive(Clone, Debug, Default)]
pub struct DiffOverlayCache {
	/// Revision the cached rows were rendered against.
	rendered_revision: u64,
	/// Monotonic cache-version counter that bumps whenever
	/// `rows` is replaced. Folded into the provider's
	/// `version()` so a cache refresh shows up in the worker's
	/// fingerprint pass.
	cache_version: u64,
	rows: Vec<VirtualRow>,
}

impl DiffOverlayCache {
	pub fn rendered_revision(&self) -> u64 {
		self.rendered_revision
	}

	pub fn cache_version(&self) -> u64 {
		self.cache_version
	}

	pub fn rows(&self) -> &[VirtualRow] {
		&self.rows
	}
}

/// One provider per active diff session.
///
/// Holds an `Arc<DiffSession>` (RCU reads of
/// `session.current_hunks()` are lock-free and stay coherent
/// even if the registry has dropped the session entry) plus a
/// shared cache of rendered rows. The cache is populated by
/// [`DiffOverlayRefreshTask`] off the worker thread; `collect()`
/// only reads.
#[derive(Debug)]
pub struct DiffOverlayVirtualRowProvider {
	session: Arc<DiffSession>,
	cache: Arc<Mutex<DiffOverlayCache>>,
}

impl DiffOverlayVirtualRowProvider {
	pub fn new(session: Arc<DiffSession>) -> Self {
		Self {
			session,
			cache: Arc::new(Mutex::new(DiffOverlayCache::default())),
		}
	}

	pub fn session(&self) -> &Arc<DiffSession> {
		&self.session
	}

	/// D.3.b (2026-05-29): pure sync render — walk the session's
	/// current `HunkIndex`, snapshot the baseline rope, render
	/// each baseline-deleted line into a `Vec<Cell>`, return
	/// the resulting `Vec<VirtualRow>` plus the revision they
	/// were rendered against. Public so tests can exercise the
	/// render path without spinning up the async refresh task.
	///
	/// D.3.b.2 (2026-05-29): `syntax` provides a `Lang`,
	/// `LangRegistry`, and `Theme` for one-shot tree-sitter
	/// highlighting of the baseline rope. When `None`, cells
	/// emit with `fg = 0` and the renderer falls back to
	/// monochrome — backward-compatible with D.3.b's behavior.
	pub fn render_rows(
		session: &DiffSession,
		baseline: &dyn BaselineSource,
		syntax: Option<&SyntaxContext>,
	) -> (u64, Vec<VirtualRow>) {
		let hunks = session.current_hunks();
		let revision = hunks.revision;
		if hunks.hunks.is_empty() {
			return (revision, Vec::new());
		}
		let baseline_rope = baseline.snapshot();
		// D.3.b.2: run one-shot tree-sitter parse once for
		// the whole baseline, then look up spans per
		// deletion-block line during the hunk walk below.
		let per_line_spans: Option<Vec<Vec<lattice_syntax::StyledSpan>>> =
			syntax.and_then(|ctx| {
				let source = baseline_rope.to_string();
				let line_count = baseline_rope.len_lines() as u32;
				lattice_syntax::oneshot_highlight_lines(
					ctx.lang,
					ctx.registry.clone(),
					&source,
					0,
					line_count,
				)
			});
		let default_fg: u32 = syntax
			.map(|ctx| {
				let s = ctx.theme.syntax_style(lattice_syntax::Style::Default);
				s.fg.map(|c| c.to_rgb_u32(0)).unwrap_or(0)
			})
			.unwrap_or(0);
		let mut rows: Vec<VirtualRow> = Vec::new();
		for hunk in &hunks.hunks {
			let (baseline_range, current_anchor) = match hunk.kind {
				HunkKind::Remove | HunkKind::Change | HunkKind::Conflict => {
					let b = match hunk.ranges.first() {
						Some(r) if r.len() > 0 => *r,
						_ => continue,
					};
					let anchor = hunk
						.ranges
						.get(1)
						.map(|r| r.start)
						.unwrap_or_else(|| b.start);
					(b, anchor)
				}
				HunkKind::Add => continue,
			};
			for line_idx in baseline_range.start..baseline_range.end {
				let cells = render_baseline_line(
					&baseline_rope,
					line_idx,
					syntax,
					per_line_spans.as_ref(),
					default_fg,
				);
				rows.push(VirtualRow {
					anchor_line: current_anchor,
					position: AnchorPosition::Above,
					cells: Arc::from(cells),
					height: 1,
					// D.6.i: deletion blocks render with the
					// diff-deletion-block backdrop.
					kind: lattice_cells::VirtualRowKind::DeletionBlock,
				});
			}
		}
		(revision, rows)
	}

}

/// D.3.b.2 (2026-05-29): the per-session syntax context the
/// provider needs to populate `Cell.fg` with theme-resolved
/// token colours.
///
/// The refresh task threads this through from
/// `DiffOverlayRefreshTask::spawn` so the provider doesn't
/// have to plumb the host's syntax / theme types into its
/// public surface. `None` (passed as `syntax: Option<&...>`
/// in [`DiffOverlayVirtualRowProvider::render_rows`]) means
/// "don't syntax-highlight" — useful for tests and for
/// languages with no registered grammar.
#[derive(Clone, Debug)]
pub struct SyntaxContext {
	pub lang: lattice_syntax::Lang,
	pub registry: Arc<lattice_syntax::LangRegistry>,
	pub theme: crate::ui::theme::Theme,
}

/// D.3.b: render one source line of `rope` as a sequence of
/// `Cell`s. `line_idx` is bounds-checked; out-of-range lines
/// produce an empty cell list (defensive against revisions
/// where the baseline rope has fewer lines than the hunk
/// expects — e.g., a session whose baseline file was
/// truncated mid-edit).
///
/// D.3.b.2 (2026-05-29): when `syntax` + `per_line_spans` are
/// supplied, each cell's `fg` is set from the styled span
/// covering its byte offset (theme-resolved RGB); bytes not
/// inside any span get `default_fg`. When `syntax` is `None`,
/// cells emit with `fg = 0` so renderers fall back to the
/// terminal / pane default foreground — the pre-D.3.b.2
/// monochrome behaviour.
fn render_baseline_line(
	rope: &ropey::Rope,
	line_idx: u32,
	syntax: Option<&SyntaxContext>,
	per_line_spans: Option<&Vec<Vec<lattice_syntax::StyledSpan>>>,
	default_fg: u32,
) -> Vec<Cell> {
	let idx = line_idx as usize;
	if idx >= rope.len_lines() {
		return Vec::new();
	}
	let line = rope.line(idx);
	let spans: &[lattice_syntax::StyledSpan] = per_line_spans
		.and_then(|p| p.get(idx))
		.map(Vec::as_slice)
		.unwrap_or(&[]);
	let theme = syntax.map(|s| &s.theme);
	let mut out: Vec<Cell> = Vec::with_capacity(line.len_chars());
	let mut byte_idx: usize = 0;
	for ch in line.chars() {
		if ch == '\n' || ch == '\r' {
			break;
		}
		// Resolve fg from the styled span covering byte_idx,
		// or fall back to default_fg / 0 per the contract
		// above.
		let fg: u32 = if let Some(theme) = theme {
			let style = spans
				.iter()
				.find(|s| {
					let start = s.start as usize;
					let end = s.end as usize;
					start <= byte_idx && byte_idx < end
				})
				.map(|s| s.style)
				.unwrap_or(lattice_syntax::Style::Default);
			let s = theme.syntax_style(style);
			s.fg.map(|c| c.to_rgb_u32(0)).unwrap_or(default_fg)
		} else {
			0
		};
		out.push(Cell::new(ch as u32, fg, 0, 0));
		byte_idx += ch.len_utf8();
	}
	out
}

impl VirtualRowProvider for DiffOverlayVirtualRowProvider {
	fn id(&self) -> ProviderId {
		// `BufferId` is `u32`; the namespace prefix lives in
		// the high bits so the buffer id remains visible in
		// the low 32 (useful for debug logs).
		DIFF_OVERLAY_PROVIDER_NAMESPACE | u64::from(self.session.buffer_id().0)
	}

	fn version(&self) -> u64 {
		// Fold the session revision + the cache version so a
		// fresh render shows up in the worker's fingerprint
		// even if the underlying session revision hasn't
		// changed (e.g., if the refresh task runs to completion
		// after the session was already at this revision when
		// the provider was first registered).
		let cache = self.cache.lock().expect("DiffOverlayCache mutex poisoned");
		let session_rev = self.session.current_hunks().revision;
		// XOR is fine here — the cache_version is bumped on
		// every install_render, so any flip in either axis
		// flips the combined value.
		session_rev ^ cache.cache_version
	}

	fn collect(&self) -> Vec<VirtualRow> {
		// Non-blocking: return the cached rows.
		let cache = self.cache.lock().expect("DiffOverlayCache mutex poisoned");
		cache.rows.clone()
	}
}

/// D.3.b: refresh task that owns the off-worker rendering of
/// deletion-block cells.
///
/// At spawn time it does an initial render so the first
/// `collect()` from the worker returns content (rather than
/// the empty default cache). Thereafter it awaits the
/// session's `publish_notify` and re-renders whenever a new
/// `HunkIndex` is published. After each render it fires
/// `virtual_rows_wake` so the worker re-runs its fingerprint
/// pass and picks up the cache change.
///
/// Held by `:diff` via the returned [`JoinHandle`]; `:diffoff`
/// aborts it.
pub struct DiffOverlayRefreshTask;

impl DiffOverlayRefreshTask {
	pub fn spawn(
		session: Arc<DiffSession>,
		baseline: Arc<dyn BaselineSource>,
		cache: Arc<Mutex<DiffOverlayCache>>,
		virtual_rows_wake: Arc<tokio::sync::Notify>,
		syntax: Option<SyntaxContext>,
	) -> JoinHandle<()> {
		tokio::spawn(async move {
			// Initial render so the first `collect()` from the
			// worker returns the populated cache.
			Self::run_once(&session, &*baseline, &cache, &virtual_rows_wake, syntax.as_ref());
			let publish_notify = session.publish_notify();
			loop {
				publish_notify.notified().await;
				Self::run_once(&session, &*baseline, &cache, &virtual_rows_wake, syntax.as_ref());
			}
		})
	}

	fn run_once(
		session: &DiffSession,
		baseline: &dyn BaselineSource,
		cache: &Mutex<DiffOverlayCache>,
		virtual_rows_wake: &Arc<tokio::sync::Notify>,
		syntax: Option<&SyntaxContext>,
	) {
		let hunks = session.current_hunks();
		// D.3.d.0: derive the per-line sign classification
		// from the same `HunkIndex` revision the deletion
		// blocks are rendered against. Publishing the sign
		// map FIRST (before deletion-block rows) means a
		// renderer reading both in the same paint pass
		// sees consistent state — same revision on both.
		let sign_map = compute_diff_sign_map(&hunks);
		session.publish_sign_map(Arc::new(sign_map));
		let (rendered_revision, rows) =
			DiffOverlayVirtualRowProvider::render_rows(session, baseline, syntax);
		debug!(
			target: "lattice_host::diff::overlay",
			buffer_id = ?session.buffer_id(),
			rendered_revision,
			n_rows = rows.len(),
			"diff overlay refresh"
		);
		let mut cache = cache.lock().expect("DiffOverlayCache mutex poisoned");
		cache.rendered_revision = rendered_revision;
		cache.cache_version = cache.cache_version.wrapping_add(1);
		cache.rows = rows;
		drop(cache);
		virtual_rows_wake.notify_one();
	}
}

impl DiffOverlayVirtualRowProvider {
	/// D.3.b: expose the shared cache so [`DiffOverlayRefreshTask::spawn`]
	/// can be wired to write to the same `Mutex` the provider's
	/// `collect`/`version` read from.
	pub fn cache_handle(&self) -> Arc<Mutex<DiffOverlayCache>> {
		Arc::clone(&self.cache)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	use std::sync::Arc;

	use lattice_core::BufferId;
	use lattice_diff::{DiffAlgorithm, Hunk, HunkIndex, HunkKind, LineRange};
	use smallvec::smallvec;

	fn bid(n: u32) -> BufferId {
		BufferId(n)
	}

	fn session_with_hunks(buffer_id: BufferId, hunks: Vec<Hunk>) -> Arc<DiffSession> {
		let s = DiffSession::new(buffer_id, DiffAlgorithm::Histogram);
		s.publish(Arc::new(HunkIndex {
			hunks,
			algorithm: DiffAlgorithm::Histogram,
			revision: 1,
		}));
		Arc::new(s)
	}

	#[test]
	fn id_carries_buffer_id_in_low_bits() {
		let s = session_with_hunks(bid(0xCAFE), vec![]);
		let p = DiffOverlayVirtualRowProvider::new(s);
		let id = p.id();
		// High prefix preserved.
		assert_eq!(id & 0xFFFF_FFFF_0000_0000, DIFF_OVERLAY_PROVIDER_NAMESPACE);
		// Low 32 bits = buffer id.
		assert_eq!(id & 0xFFFF_FFFF, 0xCAFE);
	}

	#[test]
	fn version_follows_session_revision() {
		let s = session_with_hunks(bid(1), vec![]);
		let p = DiffOverlayVirtualRowProvider::new(Arc::clone(&s));
		assert_eq!(p.version(), 1);
		s.publish(Arc::new(HunkIndex {
			hunks: vec![],
			algorithm: DiffAlgorithm::Histogram,
			revision: 9,
		}));
		assert_eq!(p.version(), 9);
	}

	// D.3.b reshapes the provider so `collect()` returns the
	// cached rows populated by the off-worker refresh task.
	// The pure render path is `render_rows(session, baseline)`;
	// tests below exercise it directly so they stay sync.

	use crate::diff::subsystem::StaticBaseline;
	use ropey::Rope;

	fn render(session: &DiffSession, baseline_text: &str) -> Vec<VirtualRow> {
		let base = StaticBaseline::new(Rope::from(baseline_text));
		DiffOverlayVirtualRowProvider::render_rows(session, &base, None).1
	}

	// D.3.b.2 (2026-05-29): variant that runs the render
	// pipeline WITH a real `SyntaxContext` so tests can
	// assert per-cell fg is populated from the one-shot
	// tree-sitter parse.
	fn render_with_syntax(session: &DiffSession, baseline_text: &str) -> Vec<VirtualRow> {
		let base = StaticBaseline::new(Rope::from(baseline_text));
		let registry = lattice_syntax::LangRegistry::standard().expect("standard registry");
		let ctx = SyntaxContext {
			lang: lattice_syntax::Lang::Rust,
			registry,
			theme: crate::ui::theme::Theme::default(),
		};
		DiffOverlayVirtualRowProvider::render_rows(session, &base, Some(&ctx)).1
	}

	#[test]
	fn cells_emit_fg_zero_without_syntax_context() {
		// D.3.b.2 backward-compat: when syntax = None, cells
		// keep fg = 0 (pre-D.3.b.2 behaviour). Renderer falls
		// back to terminal / pane default foreground.
		let hunk = Hunk {
			kind: HunkKind::Remove,
			ranges: smallvec![LineRange::new(0, 1), LineRange::new(10, 10)],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let rows = render(&s, "fn main() {}\n");
		assert_eq!(rows.len(), 1);
		// Every cell's fg should be 0 (unstyled).
		for cell in rows[0].cells.iter() {
			assert_eq!(cell.fg, 0, "syntax=None must leave cells unstyled");
		}
	}

	#[test]
	fn cells_emit_per_token_fg_with_syntax_context() {
		// D.3.b.2: the rust grammar should colour the `fn`
		// keyword distinct from the `main` identifier. The
		// exact RGB values depend on the theme, but they
		// must differ between the keyword and identifier
		// cells.
		let hunk = Hunk {
			kind: HunkKind::Remove,
			ranges: smallvec![LineRange::new(0, 1), LineRange::new(10, 10)],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let rows = render_with_syntax(&s, "fn main() {}\n");
		assert_eq!(rows.len(), 1);
		let cells = &rows[0].cells;
		assert!(cells.len() >= 4, "expected at least 'fn m...' cells");
		// `f` (idx 0) and `m` (idx 3) sit in different token
		// kinds; their `fg` must differ from each other unless
		// the theme has collapsed them to the same colour
		// (which it shouldn't for a default theme).
		let fn_fg = cells[0].fg;
		let main_fg = cells[3].fg;
		assert_ne!(
			fn_fg, main_fg,
			"keyword 'fn' and identifier 'main' should have different fg colours"
		);
	}

	#[test]
	fn empty_hunks_emit_no_rows() {
		let s = session_with_hunks(bid(1), vec![]);
		assert!(render(&s, "alpha\nbeta\n").is_empty());
	}

	#[test]
	fn add_hunks_emit_no_deletion_block() {
		// Add: baseline range is empty, current has lines. No
		// baseline lines to render as deleted.
		let hunk = Hunk {
			kind: HunkKind::Add,
			ranges: smallvec![LineRange::new(5, 5), LineRange::new(5, 8)],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		assert!(render(&s, "alpha\nbeta\n").is_empty());
	}

	#[test]
	fn remove_hunk_emits_one_row_per_baseline_line() {
		// Remove: baseline had 3 lines (rows 5..8), current has
		// 0. Anchor at current's start line (10). D.3.b
		// renders the baseline-line text into the row's cells.
		let hunk = Hunk {
			kind: HunkKind::Remove,
			ranges: smallvec![LineRange::new(0, 3), LineRange::new(10, 10)],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let rows = render(&s, "alpha\nbeta\ngamma\n");
		assert_eq!(rows.len(), 3);
		for row in &rows {
			assert_eq!(row.anchor_line, 10);
			assert_eq!(row.position, AnchorPosition::Above);
			assert_eq!(row.height, 1);
		}
		// D.3.b: the rendered cells encode the baseline line text.
		assert_eq!(rows[0].cells.len(), 5); // "alpha"
		assert_eq!(rows[1].cells.len(), 4); // "beta"
		assert_eq!(rows[2].cells.len(), 5); // "gamma"
	}

	#[test]
	fn change_hunk_emits_one_row_per_baseline_line() {
		// Change: baseline had 2 lines, current has 2 different
		// lines. Anchor at current's start line (20).
		let hunk = Hunk {
			kind: HunkKind::Change,
			ranges: smallvec![LineRange::new(0, 2), LineRange::new(20, 22)],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let rows = render(&s, "first\nsecond\n");
		assert_eq!(rows.len(), 2);
		assert_eq!(rows[0].anchor_line, 20);
		assert_eq!(rows[1].anchor_line, 20);
		assert_eq!(rows[0].cells.len(), 5); // "first"
		assert_eq!(rows[1].cells.len(), 6); // "second"
	}

	/// D.6.i (2026-05-31): deletion-block overlay rows
	/// carry `VirtualRowKind::DeletionBlock` so the
	/// renderer paints them with the deletion-block
	/// backdrop. Distinct from filler rows
	/// (`VirtualRowKind::Filler`) which paint with no
	/// backdrop.
	#[test]
	fn deletion_block_rows_carry_deletion_block_kind() {
		let hunk = Hunk {
			kind: HunkKind::Remove,
			ranges: smallvec![LineRange::new(0, 2), LineRange::new(5, 5)],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let rows = render(&s, "removed-1\nremoved-2\n");
		assert!(!rows.is_empty());
		for row in &rows {
			assert_eq!(
				row.kind,
				lattice_cells::VirtualRowKind::DeletionBlock,
				"deletion-block rows must be tagged DeletionBlock"
			);
		}
	}

	#[test]
	fn conflict_hunk_emits_deletion_block_like_change() {
		let hunk = Hunk {
			kind: HunkKind::Conflict,
			ranges: smallvec![
				LineRange::new(0, 2),
				LineRange::new(0, 2),
				LineRange::new(0, 2)
			],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let rows = render(&s, "x\ny\n");
		assert_eq!(rows.len(), 2);
	}

	#[test]
	fn out_of_range_baseline_line_renders_empty() {
		// Defensive: hunk references baseline lines past the
		// baseline rope's length (e.g., baseline file was
		// truncated between hunk compute and render). Should
		// produce empty cells, not panic.
		let hunk = Hunk {
			kind: HunkKind::Remove,
			ranges: smallvec![LineRange::new(50, 53), LineRange::new(10, 10)],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let rows = render(&s, "only one line\n");
		assert_eq!(rows.len(), 3);
		for row in &rows {
			assert_eq!(row.cells.len(), 0);
		}
	}

	#[test]
	fn collect_returns_cached_rows() {
		// D.3.b: collect() reads the cache populated by the
		// refresh task. Without a populated cache, collect()
		// returns empty regardless of hunk state.
		let hunk = Hunk {
			kind: HunkKind::Remove,
			ranges: smallvec![LineRange::new(0, 2), LineRange::new(10, 10)],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let p = DiffOverlayVirtualRowProvider::new(s);
		assert!(p.collect().is_empty(), "cache empty until refresh runs");
	}

	#[test]
	fn version_folds_session_revision_and_cache_version() {
		// Cache version starts at 0; XOR with session revision
		// 1 gives 1. After a session republish to revision 9,
		// version = 9 ^ 0 = 9.
		let s = session_with_hunks(bid(1), vec![]);
		let p = DiffOverlayVirtualRowProvider::new(Arc::clone(&s));
		assert_eq!(p.version(), 1);
		s.publish(Arc::new(HunkIndex {
			hunks: vec![],
			algorithm: DiffAlgorithm::Histogram,
			revision: 9,
		}));
		assert_eq!(p.version(), 9);
	}

	// ── D.3.d.0: DiffSignMap derivation ─────────────────────

	#[test]
	fn sign_map_empty_for_no_hunks() {
		let idx = HunkIndex {
			hunks: vec![],
			algorithm: DiffAlgorithm::Histogram,
			revision: 7,
		};
		let map = compute_diff_sign_map(&idx);
		assert!(map.is_empty());
		assert_eq!(map.revision(), 7);
		assert_eq!(map.sign_at(0), None);
	}

	#[test]
	fn add_hunk_emits_add_signs_for_each_current_line() {
		let idx = HunkIndex {
			hunks: vec![Hunk {
				kind: HunkKind::Add,
				ranges: smallvec![LineRange::new(5, 5), LineRange::new(10, 13)],
			}],
			algorithm: DiffAlgorithm::Histogram,
			revision: 1,
		};
		let map = compute_diff_sign_map(&idx);
		assert_eq!(map.len(), 3);
		assert_eq!(map.sign_at(10), Some(DiffSignKind::Add));
		assert_eq!(map.sign_at(11), Some(DiffSignKind::Add));
		assert_eq!(map.sign_at(12), Some(DiffSignKind::Add));
		assert_eq!(map.sign_at(13), None);
	}

	#[test]
	fn change_hunk_emits_change_signs_on_current_side() {
		let idx = HunkIndex {
			hunks: vec![Hunk {
				kind: HunkKind::Change,
				ranges: smallvec![LineRange::new(0, 2), LineRange::new(20, 22)],
			}],
			algorithm: DiffAlgorithm::Histogram,
			revision: 1,
		};
		let map = compute_diff_sign_map(&idx);
		assert_eq!(map.len(), 2);
		assert_eq!(map.sign_at(20), Some(DiffSignKind::Change));
		assert_eq!(map.sign_at(21), Some(DiffSignKind::Change));
		assert_eq!(map.sign_at(19), None);
	}

	#[test]
	fn remove_hunk_emits_no_current_side_signs() {
		// Remove: baseline lines disappear. The current-side
		// range is empty (start == end at the deletion
		// anchor); there are no current-side lines to sign.
		// The deletion is surfaced through the virtual-row
		// deletion block (D.3.b).
		let idx = HunkIndex {
			hunks: vec![Hunk {
				kind: HunkKind::Remove,
				ranges: smallvec![LineRange::new(5, 8), LineRange::new(10, 10)],
			}],
			algorithm: DiffAlgorithm::Histogram,
			revision: 1,
		};
		let map = compute_diff_sign_map(&idx);
		assert!(map.is_empty());
	}

	/// D.6.f (2026-05-31): three-way Conflict hunks now
	/// emit their own `DiffSignKind::Conflict` rather than
	/// collapsing into Change, so renderers can decorate
	/// them with a distinct glyph (`?`) and tint
	/// (`diff_conflict_line_bg`).
	#[test]
	fn conflict_hunk_emits_conflict_signs() {
		let idx = HunkIndex {
			hunks: vec![Hunk {
				kind: HunkKind::Conflict,
				ranges: smallvec![
					LineRange::new(0, 2),
					LineRange::new(0, 2),
					LineRange::new(0, 2)
				],
			}],
			algorithm: DiffAlgorithm::Histogram,
			revision: 1,
		};
		let map = compute_diff_sign_map(&idx);
		assert_eq!(map.len(), 2);
		assert_eq!(map.sign_at(0), Some(DiffSignKind::Conflict));
		assert_eq!(map.sign_at(1), Some(DiffSignKind::Conflict));
	}

	/// D.6.f: Conflict signs are emitted for the
	/// current-side range only (slot 1), same shape as
	/// Change. Other slots (base, remote) carry their own
	/// data but the sign map is a *current-side*
	/// decoration.
	#[test]
	fn conflict_signs_use_current_side_range() {
		// Conflict on lines 5..7 (current). base + remote
		// have different ranges in the hunk but we should
		// only see slots [5, 6] in the map.
		let idx = HunkIndex {
			hunks: vec![Hunk {
				kind: HunkKind::Conflict,
				ranges: smallvec![
					LineRange::new(10, 12), // base
					LineRange::new(5, 7),   // local / current
					LineRange::new(20, 22), // remote
				],
			}],
			algorithm: DiffAlgorithm::Histogram,
			revision: 1,
		};
		let map = compute_diff_sign_map(&idx);
		assert_eq!(map.len(), 2);
		assert_eq!(map.sign_at(5), Some(DiffSignKind::Conflict));
		assert_eq!(map.sign_at(6), Some(DiffSignKind::Conflict));
		// Base / remote slots not classified here.
		assert_eq!(map.sign_at(10), None);
		assert_eq!(map.sign_at(20), None);
	}

	/// D.6.f: mixed Change + Conflict hunks keep their
	/// distinct classifications — a renderer walking the
	/// sign map sees both kinds in the order they appear.
	#[test]
	fn mixed_change_and_conflict_keep_distinct_signs() {
		let idx = HunkIndex {
			hunks: vec![
				Hunk {
					kind: HunkKind::Change,
					ranges: smallvec![
						LineRange::new(0, 1),
						LineRange::new(0, 1),
					],
				},
				Hunk {
					kind: HunkKind::Conflict,
					ranges: smallvec![
						LineRange::new(5, 6),
						LineRange::new(5, 6),
						LineRange::new(5, 6),
					],
				},
			],
			algorithm: DiffAlgorithm::Histogram,
			revision: 1,
		};
		let map = compute_diff_sign_map(&idx);
		assert_eq!(map.sign_at(0), Some(DiffSignKind::Change));
		assert_eq!(map.sign_at(5), Some(DiffSignKind::Conflict));
	}

	#[test]
	fn sign_map_entries_sorted_by_line() {
		// Out-of-order hunks (a Change at high lines + an Add
		// at low) should still produce a sorted entry list so
		// binary search works.
		let idx = HunkIndex {
			hunks: vec![
				Hunk {
					kind: HunkKind::Change,
					ranges: smallvec![LineRange::new(0, 1), LineRange::new(100, 102)],
				},
				Hunk {
					kind: HunkKind::Add,
					ranges: smallvec![LineRange::new(5, 5), LineRange::new(10, 12)],
				},
			],
			algorithm: DiffAlgorithm::Histogram,
			revision: 1,
		};
		let map = compute_diff_sign_map(&idx);
		let lines: Vec<u32> = map.entries().iter().map(|(l, _)| *l).collect();
		assert_eq!(lines, vec![10, 11, 100, 101]);
		assert_eq!(map.sign_at(10), Some(DiffSignKind::Add));
		assert_eq!(map.sign_at(100), Some(DiffSignKind::Change));
	}

	#[test]
	fn session_sign_map_starts_empty() {
		let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
		let map = s.sign_map();
		assert!(map.is_empty());
	}

	#[test]
	fn session_publish_sign_map_round_trips() {
		let s = DiffSession::new(bid(1), DiffAlgorithm::Histogram);
		let new = compute_diff_sign_map(&HunkIndex {
			hunks: vec![Hunk {
				kind: HunkKind::Add,
				ranges: smallvec![LineRange::new(0, 0), LineRange::new(5, 7)],
			}],
			algorithm: DiffAlgorithm::Histogram,
			revision: 4,
		});
		s.publish_sign_map(Arc::new(new));
		let snap = s.sign_map();
		assert_eq!(snap.len(), 2);
		assert_eq!(snap.revision(), 4);
		assert_eq!(snap.sign_at(5), Some(DiffSignKind::Add));
	}

	#[test]
	fn multiple_hunks_merge_their_deletion_blocks() {
		// Two hunks: one Remove (2 baseline lines) at anchor
		// 10, one Change (1 baseline line) at anchor 50.
		let hunks = vec![
			Hunk {
				kind: HunkKind::Remove,
				ranges: smallvec![LineRange::new(0, 2), LineRange::new(10, 10)],
			},
			Hunk {
				kind: HunkKind::Change,
				ranges: smallvec![LineRange::new(3, 4), LineRange::new(50, 51)],
			},
		];
		let s = session_with_hunks(bid(1), hunks);
		let rows = render(&s, "a\nb\nc\nd\n");
		assert_eq!(rows.len(), 3);
		// First 2 rows from the Remove hunk anchor at 10.
		assert_eq!(rows[0].anchor_line, 10);
		assert_eq!(rows[1].anchor_line, 10);
		// Last row from the Change hunk anchors at 50.
		assert_eq!(rows[2].anchor_line, 50);
	}
}
