//! The host's [`RangeHighlightRegistrar`]: the text ranges active modes
//! tint, kept per buffer and per owning mode, and read when the active
//! buffer's render state is published (`Editor::publish_render_state`).
//! Generic — it knows no mode; `active-snippet-mode` is the first source.

use std::collections::HashMap;
use std::sync::Mutex;

use lattice_core::BufferId;
use lattice_mode::{ModeId, RangeHighlight, RangeHighlightRegistrar, RangeHighlightSource};

use crate::render_state::ModeHighlightSpan;

/// Sources by buffer, in registration order.
#[derive(Default)]
pub struct RangeHighlightRegistry {
    sources: Mutex<HashMap<BufferId, Vec<(ModeId, RangeHighlightSource)>>>,
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
                None => return Vec::new(),
            }
        };
        let len = snapshot.buffer.byte_len() as usize;
        let position = |byte: usize| snapshot.buffer.byte_to_position(byte).ok();
        sources
            .iter()
            .flat_map(|source| source(snapshot))
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
}
