//! `ai-conversation` major mode (AU‑2).
//!
//! The read-only `*ai:opencode*` buffer backing the agent conversation view.
//! Mirrors `lattice_agent::log::modes::AiLogMode`: `on_activate` reads the
//! [`ConversationStore`](crate::acp::conversation::ConversationStore) service,
//! seeds the buffer from the current snapshot, subscribes to
//! [`ConversationUpdated`](crate::acp::conversation::ConversationUpdated), and
//! spawns a drain task that re-projects on every change. The returned
//! `Subscription` guard unsubscribes on drop.
//!
//! [`render_conversation`] is pure and unit-testable. Projection is a
//! **line-granular suffix replace**: unchanged leading lines are never
//! rewritten, so streaming appends touch only the tail and a tool-call status
//! change rewrites only from its line down (AU‑2 shows status inline as text; a
//! decoration-based in-place update is a follow-up).

use lattice_mode::{
    CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    Subscription,
};

use crate::acp::conversation::{Block, Conversation, ConversationStore, ConversationUpdated, Role};

/// The synthetic buffer name for the (single, v1) opencode conversation.
pub fn conversation_buffer_name() -> String {
    "*ai:opencode*".to_string()
}

/// `ai-conversation-mode` -- major mode for the `*ai:opencode*` buffer.
pub struct AiConversationMode;

impl AiConversationMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("ai-conversation-mode")
    }
}

/// Render the whole conversation to plain text. Pure; the projection diffs this
/// against the buffer to compute a minimal edit.
pub fn render_conversation(conv: &Conversation) -> String {
    let mut out = String::new();
    for turn in &conv.turns {
        let who = match turn.role {
            Role::User => "you",
            Role::Assistant => "opencode",
        };
        out.push_str(who);
        out.push_str(":\n");
        for block in &turn.blocks {
            match block {
                Block::Text(s) => {
                    for line in s.split('\n') {
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                Block::Reasoning(s) => {
                    for line in s.split('\n') {
                        out.push_str("  \u{2502} ");
                        out.push_str(line);
                        out.push('\n');
                    }
                }
                Block::ToolCall { title, status, .. } => {
                    out.push_str("  \u{25b8} ");
                    out.push_str(title);
                    out.push_str(" [");
                    out.push_str(status_tag(status));
                    out.push_str("]\n");
                }
                Block::Edit { path, status } => {
                    out.push_str("  \u{270e} ");
                    out.push_str(path);
                    out.push_str(" [");
                    out.push_str(edit_tag(status));
                    out.push_str("]\n");
                }
            }
        }
        out.push('\n');
    }
    out
}

fn status_tag(status: &crate::acp::conversation::ToolStatus) -> &'static str {
    use crate::acp::conversation::ToolStatus::*;
    match status {
        Pending => "pending",
        Running => "running",
        Ok => "ok",
        Err => "error",
    }
}

fn edit_tag(status: &crate::acp::conversation::EditStatus) -> &'static str {
    use crate::acp::conversation::EditStatus::*;
    match status {
        Proposed => "proposed",
        Accepted => "accepted",
        Rejected => "rejected",
    }
}

/// The minimal line-granular replacement turning `old` into `new`: the index of
/// the first differing line, and the replacement text for everything from that
/// line onward. `None` when the texts are already equal.
///
/// Pure and unit-testable; the mode turns this into an
/// [`Edit`](lattice_protocol::edit::Edit) against the live buffer positions.
pub fn suffix_replace(old: &str, new: &str) -> Option<LineSuffixReplace> {
    if old == new {
        return None;
    }
    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();
    let common = old_lines
        .iter()
        .zip(new_lines.iter())
        .take_while(|(a, b)| a == b)
        .count();
    Some(LineSuffixReplace {
        first_diff_line: common,
        old_line_count: old_lines.len(),
        replacement: new_lines[common..].join("\n"),
    })
}

/// Result of [`suffix_replace`]: replace lines `first_diff_line..old_line_count`
/// (0-based, the split-on-`\n` line grid) with `replacement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineSuffixReplace {
    pub first_diff_line: usize,
    pub old_line_count: usize,
    pub replacement: String,
}

impl Mode for AiConversationMode {
    type Guard = Option<Subscription>;
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<lattice_mode::BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(None);
            };
            let Some(conv_store) = ctx.service::<ConversationStore>() else {
                return Ok(None);
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(None);
            };

            // Sync the buffer to the current snapshot with a single full replace
            // (handles a fresh OR reopened buffer uniformly). `last` then tracks
            // the buffer content so subsequent updates diff self-consistently --
            // no fragile buffer-text round-trip.
            let mut last = render_conversation(&conv_store.snapshot());
            full_replace(&handle, &last).await;

            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ConversationUpdated>();
            let sub_id = ctx.events().subscribe_typed::<ConversationUpdated>(tx);
            let bus_handle = ctx.events_handle();

            runtime.spawn(async move {
                // Coalesce a burst of updates into one re-projection.
                while rx.recv().await.is_some() {
                    while rx.try_recv().is_ok() {}
                    let new = render_conversation(&conv_store.snapshot());
                    if let Some(edit) = suffix_edit(&last, &new) {
                        let _ = handle.apply_edit_batch(vec![edit]).await;
                        last = new;
                    }
                }
            });

            Ok(Some(Subscription::new(bus_handle, sub_id)))
        })
    }
}

/// (line, col) of the end of `s` on the split-on-`\n` line grid.
fn text_end(s: &str) -> (u32, u32) {
    let last_line = s.split('\n').count().saturating_sub(1);
    let last_len = s.rsplit('\n').next().map(str::len).unwrap_or(0);
    (last_line as u32, last_len as u32)
}

/// Replace the entire buffer with `text`.
async fn full_replace(handle: &std::sync::Arc<dyn lattice_runtime::Document>, text: &str) {
    let snap = handle.snapshot();
    let last_line = snap.buffer.line_count().saturating_sub(1);
    let last_len = snap.buffer.line(last_line).unwrap_or_default().len() as u32;
    let range = lattice_protocol::Range::new(
        lattice_protocol::position::Position::new(0, 0),
        lattice_protocol::position::Position::new(last_line, last_len),
    );
    let edit = lattice_protocol::edit::Edit::replace(range, text.to_string());
    let _ = handle.apply_edit_batch(vec![edit]).await;
}

/// The minimal buffer edit turning `last` (== current buffer content) into
/// `new`, or `None` if unchanged. Positions come from `last`, which the caller
/// keeps in lockstep with the buffer -- self-consistent, no buffer round-trip.
fn suffix_edit(last: &str, new: &str) -> Option<lattice_protocol::edit::Edit> {
    let rep = suffix_replace(last, new)?;
    let (end_line, end_col) = text_end(last);
    let range = lattice_protocol::Range::new(
        lattice_protocol::position::Position::new(rep.first_diff_line as u32, 0),
        lattice_protocol::position::Position::new(end_line, end_col),
    );
    Some(lattice_protocol::edit::Edit::replace(
        range,
        rep.replacement,
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::acp::conversation::{Conversation, ToolStatus, Turn};

    fn text_turn(role: Role, text: &str) -> Turn {
        Turn {
            role,
            blocks: vec![Block::Text(text.to_string())],
        }
    }

    #[test]
    fn mode_id_is_ai_conversation_mode() {
        assert_eq!(
            AiConversationMode::mode_id(),
            ModeId::new("ai-conversation-mode")
        );
    }

    #[test]
    fn renders_turn_headers_and_blocks() {
        let conv = Conversation {
            turns: vec![
                text_turn(Role::User, "refactor parse_args"),
                Turn {
                    role: Role::Assistant,
                    blocks: vec![
                        Block::Text("I'll extract a helper.".to_string()),
                        Block::ToolCall {
                            id: "t1".to_string(),
                            title: "edit parse.rs".to_string(),
                            status: ToolStatus::Running,
                        },
                    ],
                },
            ],
        };
        let text = render_conversation(&conv);
        assert!(text.contains("you:\nrefactor parse_args\n"));
        assert!(text.contains("opencode:\nI'll extract a helper.\n"));
        assert!(text.contains("\u{25b8} edit parse.rs [running]"));
    }

    #[test]
    fn streaming_append_only_touches_the_tail() {
        let old = "opencode:\nhello\n\n";
        let new = "opencode:\nhello world\n\n";
        let rep = suffix_replace(old, new).expect("texts differ");
        // Line 0 ("opencode:") is unchanged; the first differing line is line 1.
        assert_eq!(rep.first_diff_line, 1);
        assert!(rep.replacement.starts_with("hello world"));
    }

    #[test]
    fn tool_status_change_rewrites_only_from_that_line() {
        let mut conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![
                    Block::Text("working".to_string()),
                    Block::ToolCall {
                        id: "t1".to_string(),
                        title: "run".to_string(),
                        status: ToolStatus::Running,
                    },
                ],
            }],
        };
        let before = render_conversation(&conv);
        if let Block::ToolCall { status, .. } = &mut conv.turns[0].blocks[1] {
            *status = ToolStatus::Ok;
        }
        let after = render_conversation(&conv);
        let rep = suffix_replace(&before, &after).expect("status changed");
        // "opencode:" (0), "working" (1) unchanged; the tool line (2) is first diff.
        assert_eq!(rep.first_diff_line, 2);
        assert!(rep.replacement.contains("[ok]"));
    }

    #[test]
    fn identical_render_yields_no_edit() {
        let t = "opencode:\nhi\n\n";
        assert_eq!(suffix_replace(t, t), None);
    }
}
