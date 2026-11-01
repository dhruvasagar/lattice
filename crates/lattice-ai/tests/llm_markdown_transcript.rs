//! LL.6: the chat transcript is markdown with folded blocks.
//!
//! Tool blocks are not produced by the stream until LL.16, so these build
//! `Block::ToolCall` values directly.

#![allow(clippy::unwrap_used, clippy::panic)]
#![cfg(feature = "llm")]

use std::sync::Arc;

use lattice_agent::SessionKey;
use lattice_ai::conversation::tool_fold::{
    ConversationFoldKind, ReasoningFoldSource, ToolCallFoldSource, tool_fold_identity,
};
use lattice_ai::conversation::transcript::{PROMPT_MARKER, prompt_anchor_line};
use lattice_ai::conversation::{
    Block, Conversation, ConversationStore, EditStatus, FoldDisplay, PermissionKind,
    PermissionOption, PermissionStatus, ProjectionOptions, ProjectionSource, Role, ToolKind,
    ToolStatus, Turn, project_conversation_with, render_conversation, render_conversation_with,
};
use lattice_ai::llm::config::LlmConfig;
use lattice_core::{BufferId, FoldSource};

const MD: ProjectionOptions = ProjectionOptions {
    markdown: true,
    reasoning: FoldDisplay::Closed,
    tools: FoldDisplay::Closed,
};

fn tool(id: &str, origin: Option<&str>, kind: ToolKind, input: &str, output: &str) -> Block {
    Block::ToolCall {
        id: id.into(),
        origin: origin.map(str::to_string),
        title: "search_issues".into(),
        status: ToolStatus::Ok,
        kind,
        input: Some(input.into()),
        output: Some(output.into()),
    }
}

fn assistant(blocks: Vec<Block>) -> Conversation {
    Conversation {
        turns: vec![Turn {
            role: Role::Assistant,
            blocks,
        }],
        ..Default::default()
    }
}

/// True when every fence the text opens is closed again by its end.
fn fences_balanced(text: &str) -> bool {
    let mut open: Option<(char, usize)> = None;
    for line in text.lines() {
        let Some(ch) = line.chars().next().filter(|c| *c == '`' || *c == '~') else {
            continue;
        };
        let run = line.chars().take_while(|c| *c == ch).count();
        if run < 3 {
            continue;
        }
        match open {
            Some((c, n)) if c == ch && run >= n && line[run..].trim().is_empty() => open = None,
            Some(_) => {}
            None => open = Some((ch, run)),
        }
    }
    open.is_none()
}

#[test]
fn a_tool_block_is_a_head_line_over_fenced_bodies_with_the_origin_label() {
    let conv = assistant(vec![tool(
        "t1",
        Some("github"),
        ToolKind::Search,
        "{ \"query\": \"fold overlap\" }",
        "{ \"total_count\": 3 }",
    )]);
    let (text, folds) = project_conversation_with(&conv, &MD);
    assert_eq!(
        text,
        "opencode:\n\n\u{25b8} github \u{b7} search_issues [ok]\n```json\n{ \"query\": \"fold overlap\" }\n```\n```json\n{ \"total_count\": 3 }\n```\n\n"
    );
    assert_eq!(folds.len(), 1);
    let lines: Vec<&str> = text.split('\n').collect();
    assert!(lines[folds[0].start_line as usize].starts_with('\u{25b8}'));
    assert_eq!(
        lines[folds[0].end_line as usize], "```",
        "fold ends on the last fence"
    );
    assert_eq!(folds[0].kind, ConversationFoldKind::ToolCall);
}

#[test]
fn output_language_comes_from_the_tool_then_json_then_text() {
    let lang = |kind, output: &str| {
        let text =
            render_conversation_with(&assistant(vec![tool("t", None, kind, "{}", output)]), &MD);
        // The second fence opener is the output's.
        text.lines()
            .filter(|l| l.starts_with("```") && l.len() > 3)
            .nth(1)
            .unwrap()
            .to_string()
    };
    assert_eq!(lang(ToolKind::Edit, "--- a\n+++ b"), "```diff");
    assert_eq!(lang(ToolKind::Execute, "ls"), "```bash");
    assert_eq!(lang(ToolKind::Read, "{\"a\":1}"), "```json");
    assert_eq!(lang(ToolKind::Read, "plain words"), "```text");
}

#[test]
fn a_fence_inside_tool_output_cannot_close_the_blocks_fence() {
    let body = "before\n```rust\nfn x() {}\n```\n````\nafter";
    let conv = assistant(vec![tool("t", None, ToolKind::Read, "{}", body)]);
    let text = render_conversation_with(&conv, &MD);
    assert!(
        text.contains("`````text\nbefore"),
        "fence outgrows content: {text}"
    );
    assert!(fences_balanced(&text), "{text}");
}

#[test]
fn an_unclosed_fence_in_a_reply_is_closed_so_the_prompt_is_not_code() {
    let conv = assistant(vec![Block::Text("here:\n```rust\nfn half(".into())]);
    let text = render_conversation_with(&conv, &MD);
    assert_eq!(text, "opencode:\n\nhere:\n```rust\nfn half(\n```\n\n");
    assert!(fences_balanced(&format!("{text}{PROMPT_MARKER}")));
    // The projection added a line; the prompt anchor follows the projected text.
    let anchor = prompt_anchor_line(&text);
    let full = format!("{text}{PROMPT_MARKER}");
    assert_eq!(full.split('\n').nth(anchor as usize), Some(PROMPT_MARKER));
    // Without markdown the text is untouched (ACP).
    assert_eq!(
        render_conversation(&conv),
        "opencode:\nhere:\n```rust\nfn half(\n\n"
    );
}

#[test]
fn tilde_and_longer_fences_close_with_their_own_character_and_length() {
    let conv = assistant(vec![Block::Text("~~~~\nraw".into())]);
    assert!(render_conversation_with(&conv, &MD).contains("raw\n~~~~\n"));
    let conv = assistant(vec![Block::Text("````md\n```\ninner".into())]);
    assert!(render_conversation_with(&conv, &MD).contains("inner\n````\n"));
    // Already closed: nothing added.
    let conv = assistant(vec![Block::Text("```\nx\n```".into())]);
    assert_eq!(
        render_conversation_with(&conv, &MD),
        "opencode:\n\n```\nx\n```\n\n"
    );
}

#[test]
fn a_fence_in_reasoning_stays_in_its_prefixed_block_and_a_queued_one_is_closed() {
    let mut conv = assistant(vec![Block::Reasoning("think\n```py\nx = 1".into())]);
    conv.queued.push("```\nunfinished".into());
    let text = render_conversation_with(&conv, &MD);
    assert!(
        text.contains("  \u{2502} ```py\n"),
        "reasoning keeps its prefix: {text}"
    );
    assert!(
        text.contains("you (queued):\n\n```\nunfinished\n```\n"),
        "{text}"
    );
    for l in text.lines().filter(|l| l.contains("```py")) {
        assert!(l.starts_with("  "), "not a fence opener: {l:?}");
    }
    assert!(fences_balanced(&format!("{text}{PROMPT_MARKER}")));
}

#[test]
fn hidden_leaves_blocks_out_of_the_projection_and_closed_restores_them() {
    let conv = assistant(vec![
        Block::Reasoning("a\nb".into()),
        tool("t1", None, ToolKind::Read, "{}", "{}"),
        Block::Text("done".into()),
    ]);
    let hidden = ProjectionOptions {
        reasoning: FoldDisplay::Hidden,
        tools: FoldDisplay::Hidden,
        ..MD
    };
    let (text, folds) = project_conversation_with(&conv, &hidden);
    assert_eq!(text, "opencode:\n\ndone\n\n");
    assert!(folds.is_empty());
    // The conversation itself is untouched, so closed brings everything back.
    let (text, folds) = project_conversation_with(&conv, &MD);
    assert!(text.contains("\u{2502} a") && text.contains("search_issues"));
    assert_eq!(folds.len(), 2);
}

#[test]
fn a_hidden_reasoning_block_does_not_renumber_the_ones_after_it() {
    let conv = assistant(vec![
        Block::Reasoning("one\ntwo".into()),
        Block::Text("x".into()),
    ]);
    let shown = project_conversation_with(&conv, &MD).1;
    let mut two = conv.clone();
    two.turns.push(Turn {
        role: Role::Assistant,
        blocks: vec![Block::Reasoning("three\nfour".into())],
    });
    let both = project_conversation_with(&two, &MD).1;
    assert_eq!(
        both[0].identity, shown[0].identity,
        "earlier identity is stable"
    );
    assert_eq!(both.len(), 2);
}

#[test]
fn open_shows_unfolded_and_the_option_is_read_per_recompute() {
    let store = ConversationStore::new(Arc::new(|_| {}));
    store.mutate(&SessionKey::new("llm", 1), |c| {
        c.extend_reasoning("a\nb");
        c.push_tool_call(
            "t1".into(),
            None,
            "bash".into(),
            ToolStatus::Ok,
            ToolKind::Execute,
            Some("{}".into()),
            Some("ok".into()),
        );
    });
    let mode = Arc::new(std::sync::Mutex::new(FoldDisplay::Closed));
    let source = {
        let mode = mode.clone();
        ProjectionSource::new(move || {
            let shown = *mode.lock().unwrap();
            ProjectionOptions {
                markdown: true,
                reasoning: shown,
                tools: shown,
            }
        })
    };
    let tools = ToolCallFoldSource::new(store.clone(), BufferId(1)).with_options(source.clone());
    let reasoning = ReasoningFoldSource::new(store, BufferId(1)).with_options(source);
    assert!(tools.compute_folds()[0].closed);
    *mode.lock().unwrap() = FoldDisplay::Open;
    assert!(!tools.compute_folds()[0].closed);
    assert!(!reasoning.compute_folds()[0].closed);
    *mode.lock().unwrap() = FoldDisplay::Hidden;
    assert!(tools.compute_folds().is_empty() && reasoning.compute_folds().is_empty());
}

#[test]
fn an_expanded_block_keeps_its_identity_as_text_streams_above_it() {
    let at = |preamble: &str| {
        let conv = assistant(vec![
            Block::Text(preamble.into()),
            tool("t1", None, ToolKind::Read, "{}", "{}"),
        ]);
        project_conversation_with(&conv, &MD).1
    };
    let (short, tall) = (at("one"), at("one\ntwo\nthree\nfour"));
    assert_ne!(short[0].start_line, tall[0].start_line, "the range moved");
    assert_eq!(short[0].identity, tall[0].identity);
    assert_eq!(short[0].identity, tool_fold_identity("t1"));
}

#[test]
fn the_fold_options_are_read_from_config_and_a_bad_value_warns() {
    let cfg = |s: &str| LlmConfig::from_table(&s.parse::<toml::Table>().unwrap());
    let c = cfg("[llm.fold]\nreasoning = \"open\"\ntools = \"hidden\"\n");
    assert_eq!(c.fold_reasoning, FoldDisplay::Open);
    assert_eq!(c.fold_tools, FoldDisplay::Hidden);
    assert!(c.projection_options().markdown);
    let c = cfg("[llm.fold]\ntools = \"folded\"\n");
    assert_eq!(c.fold_tools, FoldDisplay::Closed, "default kept");
    assert!(c.warnings.iter().any(|w| w.contains("llm.fold.tools")));
    let c = cfg("[llm.fold]\ntools = 3\ntool = \"open\"\n");
    assert_eq!(
        c.fold_tools,
        FoldDisplay::Closed,
        "non-string keeps default"
    );
    assert!(c.warnings.iter().any(|w| w.contains("llm.fold.tools")));
    assert!(
        c.warnings.iter().any(|w| w.contains("llm.fold.tool")),
        "unknown key"
    );
    let c = cfg("");
    assert_eq!(
        (c.fold_reasoning, c.fold_tools),
        (FoldDisplay::Closed, FoldDisplay::Closed)
    );
}

#[test]
fn acp_display_is_unchanged_by_the_new_options() {
    let conv = assistant(vec![tool("t1", Some("fs"), ToolKind::Read, "{\n}", "x")]);
    assert_eq!(
        render_conversation(&conv),
        "opencode:\n  \u{25b8} search_issues [ok]\n    input:\n      {\n      }\n    output:\n      x\n\n"
    );
}

#[test]
fn json_is_picked_by_the_first_byte_without_parsing() {
    let lang = |output: &str| {
        let text = render_conversation_with(
            &assistant(vec![tool("t", None, ToolKind::Read, "{}", output)]),
            &MD,
        );
        text.lines()
            .filter(|l| l.starts_with("```") && l.len() > 3)
            .nth(1)
            .unwrap()
            .to_string()
    };
    assert_eq!(
        lang("  \n[1, 2"),
        "```json",
        "no parse: truncated JSON still counts"
    );
    assert_eq!(lang("{not json"), "```json");
    assert_eq!(lang("42"), "```text");
    assert_eq!(lang("done {x}"), "```text");
}

#[test]
fn a_newline_in_a_tool_title_or_origin_cannot_break_the_head_line() {
    let conv = assistant(vec![Block::ToolCall {
        id: "t".into(),
        origin: Some("git\nhub".into()),
        title: "a\r\nb\nc".into(),
        status: ToolStatus::Ok,
        kind: ToolKind::Read,
        input: Some("{}".into()),
        output: None,
    }]);
    let text = render_conversation_with(&conv, &MD);
    assert_eq!(
        text.lines().nth(2),
        Some("\u{25b8} git hub \u{b7} a  b c [ok]")
    );
}

#[test]
fn a_dash_line_under_a_label_cannot_make_the_label_a_heading() {
    let user = |t: &str| Conversation {
        turns: vec![Turn {
            role: Role::User,
            blocks: vec![Block::Text(t.into())],
        }],
        queued: vec![t.into()],
        ..Default::default()
    };
    let md = render_conversation_with(&user("---"), &MD);
    assert_eq!(md, "you:\n\n---\n\nyou (queued):\n\n---\n\n");
    // ACP form is unchanged: no blank line.
    let acp = render_conversation(&user("---"));
    assert_eq!(acp, "you:\n---\n\nyou (queued):\n---\n\n");
}

#[test]
fn acp_golden_over_permission_edit_queued_and_error_blocks() {
    let conv = Conversation {
        turns: vec![
            Turn {
                role: Role::User,
                blocks: vec![Block::Text("fix it".into())],
            },
            Turn {
                role: Role::Assistant,
                blocks: vec![
                    Block::Permission {
                        id: "p1".into(),
                        title: "Allow cargo test?".into(),
                        description: Some("runs the suite".into()),
                        options: vec![
                            PermissionOption::new("a", "Allow once", PermissionKind::AllowOnce),
                            PermissionOption::new("r", "Reject", PermissionKind::RejectOnce),
                        ],
                        status: PermissionStatus::Pending,
                    },
                    Block::Edit {
                        path: "src/lib.rs".into(),
                        status: EditStatus::Proposed,
                    },
                ],
            },
        ],
        queued: vec!["and then?".into()],
        error: Some("boom\nsecond".into()),
        ..Default::default()
    };
    assert_eq!(
        render_conversation(&conv),
        "you:\nfix it\n\nopencode:\n  \u{25cc} Allow cargo test? [pending]\n    runs the suite\n    1: Allow once\n    2: Reject\n  \u{270e} src/lib.rs [proposed]\n\nyou (queued):\nand then?\n\n! boom\n  second\n\n"
    );
}
