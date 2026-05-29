//! D.3.a (2026-05-29) — `DiffOverlayVirtualRowProvider`.
//!
//! Bridges [`crate::diff_subsystem::DiffSession`]'s published
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
//! through [`crate::diff_subsystem::DiffSession::try_publish_if_newer`];
//! the `virtual_rows_worker`'s fingerprint pass picks up the
//! change on its next wake and triggers a recompute.
//! D.3.a.1's `:diff` ex-command also wires a wake forwarder
//! so a hunk publish fires the worker's `VirtualRowsWake`
//! directly — without it, the worker would only notice on the
//! next `publish_render_state` tick.

use std::sync::Arc;

use lattice_cells::{AnchorPosition, Cell, ProviderId, VirtualRow, VirtualRowProvider};
use lattice_diff::HunkKind;

use lattice_core::BufferId;

use crate::diff_subsystem::DiffSession;

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

/// One provider per active diff session.
///
/// Holds an `Arc<DiffSession>` — RCU reads of
/// `session.current_hunks()` are lock-free and stay coherent
/// even if the registry has dropped the session entry.
#[derive(Debug)]
pub struct DiffOverlayVirtualRowProvider {
	session: Arc<DiffSession>,
}

impl DiffOverlayVirtualRowProvider {
	pub fn new(session: Arc<DiffSession>) -> Self {
		Self { session }
	}

	pub fn session(&self) -> &Arc<DiffSession> {
		&self.session
	}
}

impl VirtualRowProvider for DiffOverlayVirtualRowProvider {
	fn id(&self) -> ProviderId {
		// `BufferId` is `u32`; the namespace prefix lives in
		// the high bits so the buffer id remains visible in
		// the low 32 (useful for debug logs).
		DIFF_OVERLAY_PROVIDER_NAMESPACE | u64::from(self.session.buffer_id().0)
	}

	fn version(&self) -> u64 {
		self.session.current_hunks().revision
	}

	fn collect(&self) -> Vec<VirtualRow> {
		let hunks = self.session.current_hunks();
		let mut rows: Vec<VirtualRow> = Vec::new();
		for hunk in &hunks.hunks {
			let baseline_lines = match hunk.kind {
				// D.3.a emits deletion blocks for hunks where
				// the baseline side contributes lines that
				// disappear / differ on the current side.
				HunkKind::Remove | HunkKind::Change | HunkKind::Conflict => {
					hunk.ranges.first().map(|r| r.len()).unwrap_or(0)
				}
				HunkKind::Add => 0,
			};
			if baseline_lines == 0 {
				continue;
			}
			// Current side's start line — where to anchor the
			// deletion block. `ranges[1]` is the current
			// (two-way) or local (three-way) range; we
			// anchor at its start line and emit one Above-
			// position row per deleted baseline line.
			let anchor_line = hunk
				.ranges
				.get(1)
				.map(|r| r.start)
				.unwrap_or_else(|| hunk.ranges.first().map(|r| r.start).unwrap_or(0));
			for _ in 0..baseline_lines {
				rows.push(VirtualRow {
					anchor_line,
					position: AnchorPosition::Above,
					cells: Arc::from([] as [Cell; 0]),
					height: 1,
				});
			}
		}
		rows
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

	#[test]
	fn empty_hunks_emit_no_rows() {
		let s = session_with_hunks(bid(1), vec![]);
		let p = DiffOverlayVirtualRowProvider::new(s);
		assert!(p.collect().is_empty());
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
		let p = DiffOverlayVirtualRowProvider::new(s);
		assert!(p.collect().is_empty());
	}

	#[test]
	fn remove_hunk_emits_one_row_per_baseline_line() {
		// Remove: baseline had 3 lines, current has 0. Anchor
		// at current's start line (10).
		let hunk = Hunk {
			kind: HunkKind::Remove,
			ranges: smallvec![LineRange::new(5, 8), LineRange::new(10, 10)],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let p = DiffOverlayVirtualRowProvider::new(s);
		let rows = p.collect();
		assert_eq!(rows.len(), 3);
		for row in &rows {
			assert_eq!(row.anchor_line, 10);
			assert_eq!(row.position, AnchorPosition::Above);
			assert_eq!(row.cells.len(), 0);
			assert_eq!(row.height, 1);
		}
	}

	#[test]
	fn change_hunk_emits_one_row_per_baseline_line() {
		// Change: baseline had 2 lines, current has 2 different
		// lines. Anchor at current's start line (20).
		let hunk = Hunk {
			kind: HunkKind::Change,
			ranges: smallvec![LineRange::new(7, 9), LineRange::new(20, 22)],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let p = DiffOverlayVirtualRowProvider::new(s);
		let rows = p.collect();
		assert_eq!(rows.len(), 2);
		assert_eq!(rows[0].anchor_line, 20);
		assert_eq!(rows[1].anchor_line, 20);
	}

	#[test]
	fn conflict_hunk_emits_deletion_block_like_change() {
		// Three-way Conflict: ranges[0] = base, ranges[1] =
		// local. We treat it like Change for D.3.a.
		let hunk = Hunk {
			kind: HunkKind::Conflict,
			ranges: smallvec![
				LineRange::new(0, 2),
				LineRange::new(0, 2),
				LineRange::new(0, 2)
			],
		};
		let s = session_with_hunks(bid(1), vec![hunk]);
		let p = DiffOverlayVirtualRowProvider::new(s);
		let rows = p.collect();
		assert_eq!(rows.len(), 2);
	}

	#[test]
	fn multiple_hunks_merge_their_deletion_blocks() {
		// Two hunks: one Remove (2 baseline lines) at anchor
		// 10, one Change (1 baseline line) at anchor 50.
		let hunks = vec![
			Hunk {
				kind: HunkKind::Remove,
				ranges: smallvec![LineRange::new(3, 5), LineRange::new(10, 10)],
			},
			Hunk {
				kind: HunkKind::Change,
				ranges: smallvec![LineRange::new(20, 21), LineRange::new(50, 51)],
			},
		];
		let s = session_with_hunks(bid(1), hunks);
		let p = DiffOverlayVirtualRowProvider::new(s);
		let rows = p.collect();
		assert_eq!(rows.len(), 3);
		// First 2 rows from the Remove hunk anchor at 10.
		assert_eq!(rows[0].anchor_line, 10);
		assert_eq!(rows[1].anchor_line, 10);
		// Last row from the Change hunk anchors at 50.
		assert_eq!(rows[2].anchor_line, 50);
	}
}
