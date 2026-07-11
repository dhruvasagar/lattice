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

use crate::acp::conversation::ConversationStore;
use crate::acp::conversation_mode::project_conversation;

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

/// Snapshot the store, project it, and return the `closed`-by-default folds of
/// `want` kind. Shared by both sources — the layout pass is the single source
/// of truth for line ranges.
fn folds_of_kind(store: &ConversationStore, want: ConversationFoldKind) -> Vec<Fold> {
    let conv = store.snapshot();
    let (_text, spans) = project_conversation(&conv);
    spans
        .into_iter()
        .filter(|s| s.kind == want)
        .map(|s| Fold {
            start_line: s.start_line,
            end_line: s.end_line,
            closed: true,
            identity: Some(s.identity),
        })
        .collect()
}

/// TCF: folds each tool call's captured detail rows (closed by default).
pub struct ToolCallFoldSource {
    id: ProviderId,
    store: ConversationStore,
}

impl ToolCallFoldSource {
    /// Build a source over `store`, namespaced by `buffer_id` so it is
    /// distinct from the buffer's reasoning-fold source in the registry.
    pub fn new(store: ConversationStore, buffer_id: BufferId) -> Self {
        Self {
            id: ProviderId(TOOL_FOLD_NAMESPACE | buffer_id.0 as u64),
            store,
        }
    }
}

impl FoldSource for ToolCallFoldSource {
    fn id(&self) -> ProviderId {
        self.id
    }
    fn compute_folds(&self) -> Vec<Fold> {
        folds_of_kind(&self.store, ConversationFoldKind::ToolCall)
    }
}

/// TCF: folds each multi-line reasoning block (closed by default).
pub struct ReasoningFoldSource {
    id: ProviderId,
    store: ConversationStore,
}

impl ReasoningFoldSource {
    /// Build a source over `store`, namespaced by `buffer_id`.
    pub fn new(store: ConversationStore, buffer_id: BufferId) -> Self {
        Self {
            id: ProviderId(REASONING_FOLD_NAMESPACE | buffer_id.0 as u64),
            store,
        }
    }
}

impl FoldSource for ReasoningFoldSource {
    fn id(&self) -> ProviderId {
        self.id
    }
    fn compute_folds(&self) -> Vec<Fold> {
        folds_of_kind(&self.store, ConversationFoldKind::Reasoning)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::acp::conversation::{Block, Conversation, Role, ToolStatus, Turn};
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
        assert_ne!(tool.id(), reasoning.id(), "distinct ids so removal is independent");
    }

    /// A tool call with captured detail yields exactly one closed fold from the
    /// tool source (and none from the reasoning source); its identity is keyed
    /// on the tool-call id.
    #[test]
    fn tool_source_emits_one_closed_fold_for_a_detailed_tool_call() {
        // Drive the store through the wire path so `compute_folds` reads it.
        use agent_client_protocol::schema::v1::{SessionUpdate, ToolCall as AcpToolCall};
        use lattice_agent::SessionKey;
        let store = ConversationStore::new(Arc::new(|_| {}));
        let mut tc = AcpToolCall::new("tc-1", "bash");
        tc.raw_input = Some(serde_json::json!({ "cmd": "echo hi" }));
        tc.raw_output = Some(serde_json::json!("hi\n"));
        store.apply(&SessionKey::new("opencode", 1), &SessionUpdate::ToolCall(tc));

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
        use agent_client_protocol::schema::v1::{SessionUpdate, ToolCall as AcpToolCall};
        use lattice_agent::SessionKey;
        let store = ConversationStore::new(Arc::new(|_| {}));
        store.apply(
            &SessionKey::new("opencode", 1),
            &SessionUpdate::ToolCall(AcpToolCall::new("tc-1", "think")),
        );
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
        assert_eq!(f_short.identity, tool_fold_identity("tc-1"), "identity is the id hash");
        // Sanity: the extra preamble lines pushed the fold down (otherwise the
        // test proves nothing).
        assert_eq!(
            f_tall.start_line,
            f_short.start_line + 3,
            "three extra text lines shift the fold down by three",
        );
    }
}
