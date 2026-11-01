//! Projection of a [`Conversation`] to buffer text and fold spans (AU‑2, TCF).
//!
//! Pure and protocol-neutral: the `ai-conversation` mode diffs the text this
//! produces into its buffer, and the fold sources in `tool_fold` read the spans.

use crate::conversation::model::{
    Block, Conversation, EditStatus, PermissionStatus, Role, ToolStatus,
};
use crate::conversation::tool_fold::{
    ConversationFold, ConversationFoldKind, reasoning_fold_identity, tool_fold_identity,
};

/// Render the whole conversation to plain text. Pure; the projection diffs this
/// against the buffer to compute a minimal edit. Thin wrapper over
/// [`project_conversation`] — the text half.
pub fn render_conversation(conv: &Conversation) -> String {
    project_conversation(conv).0
}

/// TCF: project the conversation to `(transcript_text, fold_spans)` in a single
/// pass, so a fold's line range can never drift from the rendered layout. The
/// text half is what the drain diffs into the buffer; the span half is what the
/// [`ToolCallFoldSource`](crate::conversation::tool_fold::ToolCallFoldSource) and
/// [`ReasoningFoldSource`](crate::conversation::tool_fold::ReasoningFoldSource) read.
///
/// A tool call with captured `input`/`output` gains indented detail rows under
/// its `▸ summary [status]` head; the fold spans the head through the last
/// detail row (head stays visible when closed, detail hides). Each multi-line
/// reasoning block folds the same way. Detail-less tool calls and single-line
/// reasoning yield no span — a 1-line fold is a `z*` no-op.
///
/// Fold line indices are 0-based on the split-on-`\n` grid and align with the
/// buffer's line numbers: the transcript occupies buffer lines `0..N` and the
/// prompt marker is appended after it, so it never shifts a transcript line.
pub fn project_conversation(conv: &Conversation) -> (String, Vec<ConversationFold>) {
    let mut out = String::new();
    let mut folds: Vec<ConversationFold> = Vec::new();
    // Current line index == number of `\n` pushed so far. Bumped on every line
    // written, so `line` names the row the NEXT push lands on.
    let mut line: u32 = 0;
    // Reasoning blocks carry no wire id; their fold identity is keyed on this
    // document-order ordinal, incremented for every reasoning block regardless
    // of whether it is foldable so a given block's ordinal stays stable.
    let mut reasoning_ordinal: usize = 0;
    for turn in &conv.turns {
        let who = match turn.role {
            Role::User => "you",
            Role::Assistant => "opencode",
        };
        out.push_str(who);
        out.push_str(":\n");
        line += 1;
        for block in &turn.blocks {
            match block {
                Block::Text(s) => {
                    for l in s.split('\n') {
                        out.push_str(l);
                        out.push('\n');
                        line += 1;
                    }
                }
                Block::Reasoning(s) => {
                    let start = line;
                    for l in s.split('\n') {
                        out.push_str("  \u{2502} ");
                        out.push_str(l);
                        out.push('\n');
                        line += 1;
                    }
                    let end = line - 1;
                    if end > start {
                        folds.push(ConversationFold {
                            start_line: start,
                            end_line: end,
                            identity: reasoning_fold_identity(reasoning_ordinal),
                            kind: ConversationFoldKind::Reasoning,
                        });
                    }
                    reasoning_ordinal += 1;
                }
                Block::ToolCall {
                    id,
                    title,
                    status,
                    input,
                    output,
                    ..
                } => {
                    let start = line;
                    out.push_str("  \u{25b8} ");
                    out.push_str(title);
                    out.push_str(" [");
                    out.push_str(status_tag(status));
                    out.push_str("]\n");
                    line += 1;
                    for (label, body) in [("input", input), ("output", output)] {
                        let Some(body) = body else { continue };
                        out.push_str("    ");
                        out.push_str(label);
                        out.push_str(":\n");
                        line += 1;
                        for l in body.split('\n') {
                            out.push_str("      ");
                            out.push_str(l);
                            out.push('\n');
                            line += 1;
                        }
                    }
                    let end = line - 1;
                    if end > start {
                        folds.push(ConversationFold {
                            start_line: start,
                            end_line: end,
                            identity: tool_fold_identity(id),
                            kind: ConversationFoldKind::ToolCall,
                        });
                    }
                }
                Block::Edit { path, status } => {
                    out.push_str("  \u{270e} ");
                    out.push_str(path);
                    out.push_str(" [");
                    out.push_str(edit_tag(status));
                    out.push_str("]\n");
                    line += 1;
                }
                Block::Permission {
                    title,
                    description,
                    options,
                    status,
                    ..
                } => {
                    out.push_str("  ");
                    out.push_str(permission_prefix(status));
                    out.push_str(title);
                    out.push_str(" [");
                    out.push_str(permission_tag(status));
                    out.push_str("]\n");
                    line += 1;
                    if let Some(desc) = description {
                        out.push_str("    ");
                        out.push_str(desc);
                        out.push('\n');
                        line += 1;
                    }
                    if status == &PermissionStatus::Pending {
                        // PU-B.2: the inline block is a record, not the
                        // interaction surface — the popup menu (`ai-permission-mode`)
                        // owns the numbered selectors. List the offered options
                        // without the old `(a)/(A)/(r)/(R)` key hints, which named
                        // chords that were never bound.
                        for (i, opt) in options.iter().enumerate() {
                            out.push_str(&format!("    {}: {}\n", i + 1, opt.name));
                            line += 1;
                        }
                    }
                }
            }
        }
        out.push('\n');
        line += 1;
    }
    (out, folds)
}

fn status_tag(status: &ToolStatus) -> &'static str {
    use ToolStatus::*;
    match status {
        Pending => "pending",
        Running => "running",
        Ok => "ok",
        Err => "error",
    }
}

fn edit_tag(status: &EditStatus) -> &'static str {
    use EditStatus::*;
    match status {
        Proposed => "proposed",
        Accepted => "accepted",
        Rejected => "rejected",
    }
}

fn permission_prefix(status: &PermissionStatus) -> &'static str {
    use PermissionStatus::*;
    match status {
        Pending => "\u{25cc} ", // ◌
        Allowed => "\u{2713} ", // ✓
        Denied => "\u{2717} ",  // ✗
    }
}

fn permission_tag(status: &PermissionStatus) -> &'static str {
    use PermissionStatus::*;
    match status {
        Pending => "pending",
        Allowed => "allowed",
        Denied => "denied",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::conversation::PermissionStatus as PS;
    use crate::conversation::model::{PermissionKind, PermissionOption, Turn};

    fn text_turn(role: Role, text: &str) -> Turn {
        Turn {
            role,
            blocks: vec![Block::Text(text.to_string())],
        }
    }

    fn test_permission_option(
        id: &'static str,
        name: &'static str,
        kind: PermissionKind,
    ) -> PermissionOption {
        PermissionOption::new(id, name, kind)
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
                            origin: None,
                            title: "edit parse.rs".to_string(),
                            status: ToolStatus::Running,
                            kind: Default::default(),
                            input: None,
                            output: None,
                        },
                    ],
                },
            ],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("you:\nrefactor parse_args\n"));
        assert!(text.contains("opencode:\nI'll extract a helper.\n"));
        assert!(text.contains("\u{25b8} edit parse.rs [running]"));
    }

    // ── TCF: tool-call / reasoning fold projection ──

    fn detailed_tool_call() -> Block {
        Block::ToolCall {
            id: "tc-1".to_string(),
            origin: None,
            title: "bash".to_string(),
            status: ToolStatus::Ok,
            kind: Default::default(),
            input: Some("{\n  \"cmd\": \"echo hi\"\n}".to_string()),
            output: Some("\"hi\"".to_string()),
        }
    }

    /// A detailed tool call renders indented `input:` / `output:` rows beneath
    /// its summary, and yields one fold spanning the summary head through the
    /// last detail row — keyed on the tool-call id. The fold's `start_line`
    /// lands on the `▸` summary line in the rendered text (head stays visible
    /// when closed; the detail rows below hide).
    #[test]
    fn tool_call_detail_rows_render_and_fold_from_the_summary_head() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![detailed_tool_call()],
            }],
            ..Default::default()
        };
        let (text, folds) = project_conversation(&conv);
        assert!(text.contains("\u{25b8} bash [ok]"), "summary head: {text}");
        assert!(text.contains("    input:\n"), "input label: {text}");
        assert!(
            text.contains("      \"cmd\": \"echo hi\""),
            "indented input body: {text}"
        );
        assert!(text.contains("    output:\n"), "output label: {text}");

        assert_eq!(folds.len(), 1);
        let f = &folds[0];
        assert_eq!(f.kind, ConversationFoldKind::ToolCall);
        assert_eq!(f.identity, tool_fold_identity("tc-1"));
        // The fold head is the summary line; the detail rows are its interior.
        let lines: Vec<&str> = text.split('\n').collect();
        assert!(
            lines[f.start_line as usize].contains("\u{25b8} bash [ok]"),
            "start_line is the summary head: {:?}",
            lines[f.start_line as usize],
        );
        assert!(f.end_line > f.start_line, "detail rows form the interior");
        // The last folded row is a detail row (the output body), not blank.
        assert!(
            lines[f.end_line as usize].contains("\"hi\""),
            "end is the last detail row"
        );
    }

    /// A tool call with no captured detail renders only its summary and yields
    /// no fold — a 1-line region is a `z*` no-op.
    #[test]
    fn detail_less_tool_call_yields_no_fold() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::ToolCall {
                    id: "t1".to_string(),
                    origin: None,
                    title: "think".to_string(),
                    status: ToolStatus::Running,
                    kind: Default::default(),
                    input: None,
                    output: None,
                }],
            }],
            ..Default::default()
        };
        let (_text, folds) = project_conversation(&conv);
        assert!(folds.is_empty(), "no detail rows → nothing to fold");
    }

    /// A multi-line reasoning block folds (identity keyed on its document-order
    /// ordinal); a single-line one does not.
    #[test]
    fn reasoning_folds_only_when_multiline() {
        let multi = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Reasoning(
                    "first thought\nsecond thought".to_string(),
                )],
            }],
            ..Default::default()
        };
        let (text, folds) = project_conversation(&multi);
        assert!(
            text.contains("  \u{2502} first thought"),
            "reasoning prefix: {text}"
        );
        assert_eq!(folds.len(), 1);
        assert_eq!(folds[0].kind, ConversationFoldKind::Reasoning);
        assert_eq!(folds[0].identity, reasoning_fold_identity(0));

        let single = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Reasoning("just one line".to_string())],
            }],
            ..Default::default()
        };
        assert!(
            project_conversation(&single).1.is_empty(),
            "a single reasoning line is not foldable",
        );
    }

    /// Fold line numbers are on the same split-on-`\n` grid the drain diffs and
    /// the buffer uses: `render_conversation` and `project_conversation` share
    /// one pass, so the text and the spans can never disagree.
    #[test]
    fn render_and_project_share_the_same_text() {
        let conv = Conversation {
            turns: vec![
                text_turn(Role::User, "run it"),
                Turn {
                    role: Role::Assistant,
                    blocks: vec![
                        Block::Reasoning("planning\nthe\napproach".to_string()),
                        detailed_tool_call(),
                    ],
                },
            ],
            ..Default::default()
        };
        assert_eq!(render_conversation(&conv), project_conversation(&conv).0);
    }

    // ── AUX‑1: permission block rendering tests ──

    #[test]
    fn render_permission_block_pending_shows_circle_and_options() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Permission {
                    id: "perm-1".to_string(),
                    title: "Allow cargo test?".to_string(),
                    description: None,
                    options: vec![
                        test_permission_option("a1", "Allow once", PermissionKind::AllowOnce),
                        test_permission_option("r1", "Reject", PermissionKind::RejectOnce),
                    ],
                    status: PS::Pending,
                }],
            }],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("\u{25cc} Allow cargo test? [pending]"));
        // PU-B.2: the inline record lists options without the old key hints —
        // the popup menu owns the numbered selectors.
        assert!(text.contains("1: Allow once"));
        assert!(text.contains("2: Reject"));
        assert!(!text.contains("(a)"), "no more misleading key hints");
    }

    #[test]
    fn render_permission_block_allowed_shows_checkmark() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Permission {
                    id: "perm-1".to_string(),
                    title: "Allow cargo test?".to_string(),
                    description: None,
                    options: vec![],
                    status: PS::Allowed,
                }],
            }],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("\u{2713} Allow cargo test? [allowed]"));
        assert!(!text.contains("1:"), "no options after resolution");
    }

    #[test]
    fn render_permission_block_denied_shows_x() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Permission {
                    id: "perm-1".to_string(),
                    title: "Allow cargo test?".to_string(),
                    description: None,
                    options: vec![],
                    status: PS::Denied,
                }],
            }],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("\u{2717} Allow cargo test? [denied]"));
    }
}
