//! TCF: fold sources for the `*ai:opencode*` conversation buffer.
//!
//! Two [`lattice_core::FoldSource`]s, both reading the same single-pass
//! layout ([`project_conversation`]) so a fold's line range can never drift
//! from the rendered text:
//!
//! - [`ToolCallFoldSource`] — one fold per tool call that has captured
//!   `input`/`output` detail, spanning its `▸ summary [status]` head line
//!   through the last detail row.
//! - [`ReasoningFoldSource`] — one fold per multi-line reasoning block.
//!
//! Both fold `closed: true` by default (a fresh tool call / reasoning block
//! opens collapsed; `za` on the head line expands it). Lattice fold semantics
//! keep the head line visible (`start_line < line <= end_line`, see
//! `lattice_host::folds::FoldIndex::line_inside_closed_fold`), so a closed
//! fold shows only the summary and elides the detail rows at the cell layer —
//! a collapsed transcript costs the renderer nothing per hidden line
//! (paramount #1).
//!
//! **Mode-owned**, exactly the shape `diff-mode` uses for [`HunkFoldSource`]:
//! `AiConversationMode::on_activate` constructs one of each (holding a
//! [`ConversationStore`] clone) and registers them via the
//! `FoldOverlayService`; the mode's `Drop` guard removes them. Each holds the
//! store, so `compute_folds` reads the currently-published conversation on
//! every recompute — no `FoldContext`, no buffer round-trip.
//!
//! [`HunkFoldSource`]: crate — see `lattice_diff::fold::HunkFoldSource` for the
//! precedent this mirrors.
//!
//! ## Identity — the streaming-coherence fix
//!
//! A closed fold's expansion state is carried across recomputes by
//! [`lattice_core::Fold::identity`], not by its line range. The transcript
//! grows above a tool call on every streamed token, shifting its line range;
//! keying identity on the *tool-call id* (not the range) is what keeps an
//! expanded call expanded as content lands above it. Reasoning blocks carry no
//! wire id, so their identity is keyed on the block's *ordinal* in document
//! order — stable because earlier turns are never rewritten, so a given
//! reasoning block stays the nth one as it streams.

use std::hash::{DefaultHasher, Hash, Hasher};

use lattice_core::{BufferId, Fold, FoldSource, ProviderId};

use crate::conversation::ConversationStore;
use crate::conversation::projection::{FoldDisplay, ProjectionSource, project_conversation_with};

/// Namespace for the per-buffer tool-call fold provider id. OR'd with the
/// buffer's id (low 32 bits) so the source is distinct in the registry —
/// `FoldOverlayService::add_source` keys removal on the id. Distinct high bits
/// from [`REASONING_FOLD_NAMESPACE`] so a buffer's two AI fold sources register
/// under different ids (they coexist over disjoint line regions). Distinct from
/// diff's `0xD1FF_*` and multibuffer's `0xBBBB_*` namespaces.
pub const TOOL_FOLD_NAMESPACE: u64 = 0xA1F0_0001_0000_0000;

/// Namespace for the per-buffer reasoning fold provider id. See
/// [`TOOL_FOLD_NAMESPACE`].
pub const REASONING_FOLD_NAMESPACE: u64 = 0xA1F0_0002_0000_0000;

/// TCF: which kind of block a [`ConversationFold`] covers, so each
/// [`FoldSource`] can filter [`project_conversation`]'s span list to its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConversationFoldKind {
    ToolCall,
    Reasoning,
}

/// TCF: one foldable region in the projected transcript, produced by
/// [`project_conversation`]. `start_line` is the fold head (stays visible when
/// closed); `start_line + 1 ..= end_line` are the rows that hide. `identity`
/// carries closed-state across streaming recomputes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationFold {
    pub start_line: u32,
    pub end_line: u32,
    pub identity: u64,
    pub kind: ConversationFoldKind,
}

/// Stable identity for a tool-call fold, keyed on the wire `tool_call_id` (not
/// the line range) so the fold's expansion state survives a transcript that
/// grows above it. Namespaced with `"ai:tool"` so it never collides with a
/// primary provider's hash for the same span.
pub fn tool_fold_identity(tool_call_id: &str) -> u64 {
    let mut h = DefaultHasher::new();
    "ai:tool".hash(&mut h);
    tool_call_id.hash(&mut h);
    h.finish()
}

/// Stable identity for a reasoning fold, keyed on the block's ordinal in
/// document order (reasoning blocks carry no wire id). Namespaced with
/// `"ai:reasoning"`.
pub fn reasoning_fold_identity(ordinal: usize) -> u64 {
    let mut h = DefaultHasher::new();
    "ai:reasoning".hash(&mut h);
    ordinal.hash(&mut h);
    h.finish()
}

/// One projection's folds, taken together with its text. The drain makes it from
/// the same conversation snapshot and options it rendered the text from, and the
/// fold sources read it, so text and folds cannot disagree: a source that
/// re-projected the live store on its own would see a later conversation, or
/// later options, than the text the buffer holds.
#[derive(Debug, Clone, Default)]
pub struct ProjectionFrame {
    pub options: crate::conversation::ProjectionOptions,
    pub folds: Vec<ConversationFold>,
}

/// Where the drain publishes its latest [`ProjectionFrame`]. Shared by the drain
/// and both fold sources of one buffer.
#[derive(Debug, Clone, Default)]
pub struct FrameCell(std::sync::Arc<std::sync::RwLock<Option<std::sync::Arc<ProjectionFrame>>>>);

impl FrameCell {
    pub fn publish(&self, frame: ProjectionFrame) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = Some(std::sync::Arc::new(frame));
    }

    pub fn latest(&self) -> Option<std::sync::Arc<ProjectionFrame>> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Where a fold source gets its spans: the live store (`:opencode-acp`, which
/// has no drain-owned frame) or the drain's frame.
#[derive(Clone)]
enum FoldInput {
    Live(ProjectionSource),
    Frame(FrameCell),
}

impl Default for FoldInput {
    fn default() -> Self {
        Self::Live(ProjectionSource::default())
    }
}

/// The folds of `want` kind, closed unless the options say `open`. Shared by
/// both sources: the layout pass is the single source of truth for line ranges.
/// A `hidden` kind has no spans in the projection, so it yields no folds.
fn folds_of_kind(
    store: &ConversationStore,
    input: &FoldInput,
    want: ConversationFoldKind,
) -> Vec<Fold> {
    let (opts, spans) = match input {
        FoldInput::Live(options) => {
            let conv = store.snapshot();
            let opts = options.get();
            let (_text, spans) = project_conversation_with(&conv, &opts);
            (opts, spans)
        }
        FoldInput::Frame(cell) => match cell.latest() {
            Some(frame) => (frame.options, frame.folds.clone()),
            None => return Vec::new(),
        },
    };
    let shown = match want {
        ConversationFoldKind::ToolCall => opts.tools,
        ConversationFoldKind::Reasoning => opts.reasoning,
    };
    spans
        .into_iter()
        .filter(|s| s.kind == want)
        .map(|s| Fold {
            start_line: s.start_line,
            end_line: s.end_line,
            closed: shown != FoldDisplay::Open,
            identity: Some(s.identity),
        })
        .collect()
}

/// TCF: folds each tool call's captured detail rows (closed by default).
pub struct ToolCallFoldSource {
    id: ProviderId,
    store: ConversationStore,
    input: FoldInput,
}

impl ToolCallFoldSource {
    /// Build a source over `store`, namespaced by `buffer_id` so it is
    /// distinct from the buffer's reasoning-fold source in the registry.
    pub fn new(store: ConversationStore, buffer_id: BufferId) -> Self {
        Self {
            id: ProviderId(TOOL_FOLD_NAMESPACE | buffer_id.0 as u64),
            store,
            input: FoldInput::default(),
        }
    }

    /// Read the display options from `options` on every recompute.
    pub fn with_options(mut self, options: ProjectionSource) -> Self {
        self.input = FoldInput::Live(options);
        self
    }

    /// Read the spans and options from the drain's frame instead of
    /// re-projecting the live store.
    pub fn with_frame(mut self, frame: FrameCell) -> Self {
        self.input = FoldInput::Frame(frame);
        self
    }
}

impl FoldSource for ToolCallFoldSource {
    fn id(&self) -> ProviderId {
        self.id
    }
    fn compute_folds(&self) -> Vec<Fold> {
        folds_of_kind(&self.store, &self.input, ConversationFoldKind::ToolCall)
    }
}

/// TCF: folds each multi-line reasoning block (closed by default).
pub struct ReasoningFoldSource {
    id: ProviderId,
    store: ConversationStore,
    input: FoldInput,
}

impl ReasoningFoldSource {
    /// Build a source over `store`, namespaced by `buffer_id`.
    pub fn new(store: ConversationStore, buffer_id: BufferId) -> Self {
        Self {
            id: ProviderId(REASONING_FOLD_NAMESPACE | buffer_id.0 as u64),
            store,
            input: FoldInput::default(),
        }
    }

    /// Read the display options from `options` on every recompute.
    pub fn with_options(mut self, options: ProjectionSource) -> Self {
        self.input = FoldInput::Live(options);
        self
    }

    /// Read the spans and options from the drain's frame instead of
    /// re-projecting the live store.
    pub fn with_frame(mut self, frame: FrameCell) -> Self {
        self.input = FoldInput::Frame(frame);
        self
    }
}

impl FoldSource for ReasoningFoldSource {
    fn id(&self) -> ProviderId {
        self.id
    }
    fn compute_folds(&self) -> Vec<Fold> {
        folds_of_kind(&self.store, &self.input, ConversationFoldKind::Reasoning)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::conversation::projection::project_conversation;
    use crate::conversation::{Block, Conversation, Role, ToolStatus, Turn};
    use std::sync::Arc;

    /// A detailed tool call in an assistant turn preceded by `preamble` lines of
    /// text — lets a test grow the content above the call to shift its range.
    fn conv_with_tool_after(preamble: &str) -> Conversation {
        Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![
                    Block::Text(preamble.to_string()),
                    Block::ToolCall {
                        id: "tc-1".to_string(),
                        origin: None,
                        title: "bash".to_string(),
                        status: ToolStatus::Ok,
                        kind: Default::default(),
                        input: Some("{\n  \"cmd\": \"echo hi\"\n}".to_string()),
                        output: Some("\"hi\"".to_string()),
                    },
                ],
            }],
            ..Default::default()
        }
    }

    #[test]
    fn tool_identity_stable_per_id_and_distinct_between_ids() {
        assert_eq!(tool_fold_identity("tc-1"), tool_fold_identity("tc-1"));
        assert_ne!(tool_fold_identity("tc-1"), tool_fold_identity("tc-2"));
    }

    #[test]
    fn reasoning_identity_stable_per_ordinal_and_distinct_from_tool() {
        assert_eq!(reasoning_fold_identity(0), reasoning_fold_identity(0));
        assert_ne!(reasoning_fold_identity(0), reasoning_fold_identity(1));
        // Namespaced away from tool identities so the same span never collides.
        assert_ne!(reasoning_fold_identity(0), tool_fold_identity("tc-1"));
    }

    #[test]
    fn provider_ids_are_namespaced_per_buffer_and_per_kind() {
        let store = ConversationStore::new(Arc::new(|_| {}));
        let tool = ToolCallFoldSource::new(store.clone(), BufferId(7));
        let reasoning = ReasoningFoldSource::new(store, BufferId(7));
        assert_eq!(tool.id(), ProviderId(TOOL_FOLD_NAMESPACE | 7));
        assert_eq!(reasoning.id(), ProviderId(REASONING_FOLD_NAMESPACE | 7));
        assert_ne!(
            tool.id(),
            reasoning.id(),
            "distinct ids so removal is independent"
        );
    }

    /// A tool call with captured detail yields exactly one closed fold from the
    /// tool source (and none from the reasoning source); its identity is keyed
    /// on the tool-call id.
    #[test]
    fn tool_source_emits_one_closed_fold_for_a_detailed_tool_call() {
        // Drive the store through its mutate path so `compute_folds` reads it.
        use crate::conversation::ToolKind;
        use lattice_agent::SessionKey;
        let store = ConversationStore::new(Arc::new(|_| {}));
        store.mutate(&SessionKey::new("opencode", 1), |c| {
            c.push_tool_call(
                "tc-1".into(),
                None,
                "bash".into(),
                ToolStatus::Ok,
                ToolKind::Execute,
                Some("{\n  \"cmd\": \"echo hi\"\n}".into()),
                Some("\"hi\\n\"".into()),
            );
        });

        let tool = ToolCallFoldSource::new(store.clone(), BufferId(1));
        let folds = tool.compute_folds();
        assert_eq!(folds.len(), 1, "one detailed tool call → one fold");
        assert!(folds[0].closed, "folds start closed (collapsed by default)");
        assert_eq!(folds[0].identity, Some(tool_fold_identity("tc-1")));
        assert!(
            folds[0].end_line > folds[0].start_line,
            "fold spans the summary head + detail rows",
        );

        // The reasoning source sees nothing for a tool-only transcript.
        let reasoning = ReasoningFoldSource::new(store, BufferId(1));
        assert!(reasoning.compute_folds().is_empty());
    }

    /// A detail-less tool call is not foldable — no detail rows means a 1-line
    /// region, which the `z*` grammar treats as a no-op.
    #[test]
    fn tool_source_skips_a_tool_call_without_detail() {
        use crate::conversation::ToolKind;
        use lattice_agent::SessionKey;
        let store = ConversationStore::new(Arc::new(|_| {}));
        store.mutate(&SessionKey::new("opencode", 1), |c| {
            c.push_tool_call(
                "tc-1".into(),
                None,
                "think".into(),
                ToolStatus::Pending,
                ToolKind::Other,
                None,
                None,
            );
        });
        let tool = ToolCallFoldSource::new(store, BufferId(1));
        assert!(tool.compute_folds().is_empty());
    }

    /// The streaming-coherence property: text growing ABOVE a tool call shifts
    /// its fold line range but NOT its identity, so `recompute_folds`' identity
    /// carry-over keeps an expanded call expanded. Keying identity on the line
    /// range instead would reopen it on every streamed token.
    #[test]
    fn tool_fold_identity_survives_a_transcript_growing_above_it() {
        let short = conv_with_tool_after("a");
        let tall = conv_with_tool_after("a\nb\nc\nd");

        let f_short = project_conversation(&short)
            .1
            .into_iter()
            .find(|s| s.kind == ConversationFoldKind::ToolCall)
            .expect("tool fold present");
        let f_tall = project_conversation(&tall)
            .1
            .into_iter()
            .find(|s| s.kind == ConversationFoldKind::ToolCall)
            .expect("tool fold still present");

        assert_eq!(
            f_short.identity, f_tall.identity,
            "identity is keyed on tool_call_id, not the line range",
        );
        assert_eq!(
            f_short.identity,
            tool_fold_identity("tc-1"),
            "identity is the id hash"
        );
        // Sanity: the extra preamble lines pushed the fold down (otherwise the
        // test proves nothing).
        assert_eq!(
            f_tall.start_line,
            f_short.start_line + 3,
            "three extra text lines shift the fold down by three",
        );
    }
}
