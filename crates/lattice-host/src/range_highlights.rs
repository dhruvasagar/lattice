//! The host's [`RangeHighlightRegistrar`]: the text ranges active modes
//! tint, kept per buffer and per owning mode, and read when the active
//! buffer's render state is published (`Editor::publish_render_state`).
//! Generic — it knows no mode; `active-snippet-mode` is the first source.
//!
//! RF.4d: it also tints ranges tracked through `Effect::TrackRange` — a
//! plugin's, which cannot register a source closure because that would run
//! guest code on every publish. The document keeps those ranges in place;
//! this keeps only which id gets which element, and reads where the range is
//! from the snapshot being published.

use std::collections::HashMap;
use std::sync::Mutex;

use lattice_core::BufferId;
use lattice_core::tracked::TrackedRangeId;
use lattice_mode::{ModeId, RangeHighlight, RangeHighlightRegistrar, RangeHighlightSource};
use lattice_theme::ElementId;

use crate::render_state::ModeHighlightSpan;

/// Sources by buffer, in registration order.
#[derive(Default)]
pub struct RangeHighlightRegistry {
    sources: Mutex<HashMap<BufferId, Vec<(ModeId, RangeHighlightSource)>>>,
    /// RF.4d: tinted tracked ranges by buffer.
    tracked: Mutex<HashMap<BufferId, Vec<TrackedTint>>>,
}

/// RF.4d: one tracked range's tint.
#[derive(Debug, Clone)]
struct TrackedTint {
    id: TrackedRangeId,
    element: ElementId,
    /// Where the range was when tracked, and the text version that was at.
    /// The `Track` reaches the document through its mailbox, so the publish
    /// straight after it can hold a snapshot from before; at the same text
    /// version these bytes are still right, and the tint shows at once
    /// rather than on the next keystroke.
    bytes: std::ops::Range<usize>,
    text_version: u64,
}

impl std::fmt::Debug for RangeHighlightRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RangeHighlightRegistry")
            .finish_non_exhaustive()
    }
}

impl RangeHighlightRegistrar for RangeHighlightRegistry {
    fn register(&self, buffer: BufferId, owner: ModeId, source: RangeHighlightSource) {
        let mut sources = self.sources.lock().unwrap_or_else(|e| e.into_inner());
        let list = sources.entry(buffer).or_default();
        list.retain(|(o, _)| *o != owner);
        list.push((owner, source));
    }

    fn unregister(&self, buffer: BufferId, owner: &ModeId) {
        let mut sources = self.sources.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = sources.get_mut(&buffer) {
            list.retain(|(o, _)| o != owner);
            if list.is_empty() {
                sources.remove(&buffer);
            }
        }
    }
}

impl RangeHighlightRegistry {
    /// RF.4d: tint tracked range `id` of `buffer` with `element`, replacing
    /// its tint if it has one. `bytes` / `text_version` are where it was
    /// tracked, read until the document's snapshot carries the range.
    pub fn tint_tracked(
        &self,
        buffer: BufferId,
        id: TrackedRangeId,
        element: ElementId,
        bytes: std::ops::Range<usize>,
        text_version: u64,
    ) {
        let mut tracked = self.tracked.lock().unwrap_or_else(|e| e.into_inner());
        let list = tracked.entry(buffer).or_default();
        list.retain(|t| t.id != id);
        list.push(TrackedTint {
            id,
            element,
            bytes,
            text_version,
        });
    }

    /// RF.4d: drop tracked range `id`'s tint, if it has one.
    pub fn untint_tracked(&self, buffer: BufferId, id: TrackedRangeId) {
        let mut tracked = self.tracked.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = tracked.get_mut(&buffer) {
            list.retain(|t| t.id != id);
            if list.is_empty() {
                tracked.remove(&buffer);
            }
        }
    }

    /// The tracked tints for `buffer` as `snapshot` holds the ranges. A
    /// range the snapshot does not hold is shown where it was tracked only
    /// while the text is unchanged since; a collapsed one is not shown.
    fn tracked_highlights(
        &self,
        buffer: BufferId,
        snapshot: &lattice_runtime::DocumentSnapshot,
    ) -> Vec<RangeHighlight> {
        let tracked = self.tracked.lock().unwrap_or_else(|e| e.into_inner());
        let Some(list) = tracked.get(&buffer) else {
            return Vec::new();
        };
        list.iter()
            .filter_map(|t| {
                let bytes = match snapshot.tracked_ranges.get(t.id) {
                    Some(r) if r.collapsed => return None,
                    Some(r) => r.start..r.end,
                    None if snapshot.text_version == t.text_version => t.bytes.clone(),
                    None => return None,
                };
                Some(RangeHighlight {
                    bytes,
                    element: t.element,
                })
            })
            .collect()
    }

    /// Every source's highlights for `buffer` as `snapshot` holds it, in
    /// line/byte positions for the renderers. A range that does not lie in
    /// the snapshot's text, or is empty, is dropped: a source is told the
    /// document, but a keystroke must not be what finds a bad range.
    pub fn spans_for(
        &self,
        buffer: BufferId,
        snapshot: &lattice_runtime::DocumentSnapshot,
    ) -> Vec<ModeHighlightSpan> {
        // Clone the sources out so none runs under the lock — a source may
        // take its own mode's locks.
        let sources: Vec<RangeHighlightSource> = {
            let map = self.sources.lock().unwrap_or_else(|e| e.into_inner());
            match map.get(&buffer) {
                Some(list) => list.iter().map(|(_, s)| s.clone()).collect(),
                None => Vec::new(),
            }
        };
        let tracked = self.tracked_highlights(buffer, snapshot);
        if sources.is_empty() && tracked.is_empty() {
            return Vec::new();
        }
        let len = snapshot.buffer.byte_len() as usize;
        let position = |byte: usize| snapshot.buffer.byte_to_position(byte).ok();
        sources
            .iter()
            .flat_map(|source| source(snapshot))
            .chain(tracked)
            .filter_map(|RangeHighlight { bytes, element }| {
                if bytes.start >= bytes.end || bytes.end > len {
                    return None;
                }
                Some(ModeHighlightSpan {
                    range: lattice_protocol::position::Range::new(
                        position(bytes.start)?,
                        position(bytes.end)?,
                    ),
                    element,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_theme::ElementId;
    use std::sync::Arc;

    fn snapshot(text: &str) -> lattice_runtime::DocumentSnapshot {
        lattice_runtime::DocumentSnapshot::__bench_from_document(
            &lattice_core::Document::from_text(text),
        )
    }

    /// A source tinting each `(start, end)` byte span.
    fn source(spans: &[(usize, usize)]) -> RangeHighlightSource {
        let spans = spans.to_vec();
        Arc::new(move |_| {
            spans
                .iter()
                .map(|&(start, end)| RangeHighlight {
                    bytes: start..end,
                    element: ElementId(7),
                })
                .collect()
        })
    }

    #[test]
    fn spans_are_line_positions_and_bad_ranges_are_dropped() {
        let reg = RangeHighlightRegistry::default();
        let buf = BufferId(1);
        reg.register(
            buf,
            ModeId::new("m"),
            source(&[(1, 2), (4, 6), (3, 3), (5, 99)]),
        );
        let spans = reg.spans_for(buf, &snapshot("ab\ncde\n"));
        let at = lattice_protocol::position::Position::new;
        let range = lattice_protocol::position::Range::new;
        assert_eq!(
            spans,
            vec![
                ModeHighlightSpan {
                    range: range(at(0, 1), at(0, 2)),
                    element: ElementId(7),
                },
                ModeHighlightSpan {
                    range: range(at(1, 1), at(1, 3)),
                    element: ElementId(7),
                },
            ],
            "the empty and the out-of-text ranges are dropped"
        );
    }

    #[test]
    fn unregister_withdraws_only_the_owners_source() {
        let reg = RangeHighlightRegistry::default();
        let buf = BufferId(1);
        reg.register(buf, ModeId::new("a"), source(&[(0, 1)]));
        reg.register(buf, ModeId::new("b"), source(&[(1, 2)]));
        reg.register(buf, ModeId::new("a"), source(&[(0, 2)]));
        assert_eq!(
            reg.spans_for(buf, &snapshot("abc")).len(),
            2,
            "a replaced, b kept"
        );
        reg.unregister(buf, &ModeId::new("a"));
        assert_eq!(reg.spans_for(buf, &snapshot("abc")).len(), 1);
        reg.unregister(buf, &ModeId::new("b"));
        assert!(reg.spans_for(buf, &snapshot("abc")).is_empty());
        assert!(
            reg.spans_for(BufferId(2), &snapshot("abc")).is_empty(),
            "another buffer has none"
        );
    }

    /// RF.4d: a tracked tint is read from where the snapshot holds the range
    /// now; before the document has taken the `Track`, from where it was
    /// tracked, but only while the text is unchanged since; never once
    /// collapsed, or once untinted.
    #[test]
    fn tracked_tints_follow_the_snapshot_and_fall_back_only_at_the_same_text() {
        use lattice_core::tracked::{Stickiness, TrackedRangeId};
        let (buf, id) = (BufferId(1), TrackedRangeId(1 << 63));
        let reg = RangeHighlightRegistry::default();
        let plain = snapshot("abcdef");
        reg.tint_tracked(buf, id, ElementId(9), 1..3, plain.text_version);

        let bytes = |snap: &lattice_runtime::DocumentSnapshot| -> Vec<(u32, u32)> {
            reg.spans_for(buf, snap)
                .iter()
                .map(|s| (s.range.start.byte, s.range.end.byte))
                .collect()
        };
        assert_eq!(bytes(&plain), vec![(1, 3)], "not yet tracked: where it was");

        let mut doc = lattice_core::Document::from_text("abcdef");
        doc.update_tracked_ranges(|t| t.insert(id, 2..4, Stickiness::NeverGrows));
        let moved = lattice_runtime::DocumentSnapshot::__bench_from_document(&doc);
        assert_eq!(
            bytes(&moved),
            vec![(2, 4)],
            "tracked: where the document says"
        );

        let mut later = plain.clone();
        later.text_version += 1;
        assert!(bytes(&later).is_empty(), "the text moved on: no stale tint");

        reg.untint_tracked(buf, id);
        assert!(bytes(&moved).is_empty(), "untinted");
    }
}
