//! LL.6 -- sustained projection of a growing reply.
//!
//! The drain re-projects the whole conversation on every coalesced update, so
//! the markdown projection's cost per update bounds how fast a reply can stream
//! beside typing. This projects a conversation whose last reply grows by one
//! line at a time, with tool blocks and an open fence in the history, and
//! reports the per-update cost at several reply lengths.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use lattice_ai::conversation::{
    Block, Conversation, FoldDisplay, ProjectionOptions, Role, ToolKind, ToolStatus, Turn,
    project_conversation_with,
};
use std::hint::black_box;

const OPTS: ProjectionOptions = ProjectionOptions {
    markdown: true,
    reasoning: FoldDisplay::Closed,
    tools: FoldDisplay::Closed,
};

fn history(turns: usize) -> Vec<Turn> {
    (0..turns)
        .flat_map(|i| {
            [
                Turn {
                    role: Role::User,
                    blocks: vec![Block::Text(format!("question {i}"))],
                },
                Turn {
                    role: Role::Assistant,
                    blocks: vec![
                        Block::Reasoning("weighing\nthe options".into()),
                        Block::ToolCall {
                            id: format!("t{i}"),
                            origin: Some("github".into()),
                            title: "search_issues".into(),
                            status: ToolStatus::Ok,
                            kind: ToolKind::Search,
                            input: Some("{ \"query\": \"fold overlap\" }".into()),
                            output: Some("{ \"total_count\": 3 }".into()),
                        },
                        Block::Text("An answer.\n\n```rust\nfn x() {}\n```".into()),
                    ],
                },
            ]
        })
        .collect()
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("llm_projection");
    for lines in [10usize, 200, 2000] {
        let mut turns = history(20);
        // The streaming reply, left inside an open fence like a half-received block.
        let mut reply = String::from("```rust\n");
        for i in 0..lines {
            reply.push_str(&format!("let v{i} = {i};\n"));
        }
        turns.push(Turn {
            role: Role::Assistant,
            blocks: vec![Block::Text(reply)],
        });
        let conv = Conversation {
            turns,
            ..Default::default()
        };
        group.bench_with_input(BenchmarkId::new("reply_lines", lines), &conv, |b, conv| {
            b.iter(|| black_box(project_conversation_with(black_box(conv), &OPTS)));
        });
    }
    // One 100 KB tool output: language detection must not cost a parse.
    let big = format!("[{}]", "1,".repeat(50_000));
    let conv = Conversation {
        turns: vec![Turn {
            role: Role::Assistant,
            blocks: vec![Block::ToolCall {
                id: "big".into(),
                origin: None,
                title: "dump".into(),
                status: ToolStatus::Ok,
                kind: ToolKind::Search,
                input: Some("{}".into()),
                output: Some(big),
            }],
        }],
        ..Default::default()
    };
    group.bench_function("tool_output_100kb", |b| {
        b.iter(|| black_box(project_conversation_with(black_box(&conv), &OPTS)));
    });
    group.finish();
}

/// LL.16: each round of a tool loop builds its next request by projecting the
/// conversation through `history::messages`, off the editor thread. This is
/// that cost for a turn that has run `rounds` tool calls, each with a 2 KB
/// result, so the per-round cost of a long loop is visible.
fn bench_tool_history(c: &mut Criterion) {
    use lattice_ai::conversation::ToolCallData;
    let mut group = c.benchmark_group("llm_tool_history");
    let output = "x".repeat(2048);
    for rounds in [10usize, 50, 200] {
        let mut conv = Conversation::default();
        conv.push_user_text("work on it");
        for i in 0..rounds {
            let id = format!("call_{i}");
            conv.extend_text(Role::Assistant, &format!("step {i}"));
            conv.push_tool_call(
                id.clone(),
                None,
                "read_file".into(),
                ToolStatus::Ok,
                ToolKind::Read,
                Some("{ \"path\": \"src/lib.rs\" }".into()),
                Some(output.clone()),
            );
            conv.tool_calls.insert(
                id,
                ToolCallData {
                    name: "lattice__read_file".into(),
                    input: serde_json::json!({"path": "src/lib.rs"}),
                    malformed: None,
                },
            );
        }
        group.bench_with_input(BenchmarkId::new("rounds", rounds), &conv, |b, conv| {
            b.iter(|| black_box(lattice_ai::llm::history::messages(black_box(conv))));
        });
    }
    group.finish();
}

/// LL.17: an accepted edit is applied as the span that differs, found by one
/// pass over the file from each end. One line changed mid-file.
fn bench_edit_span(c: &mut Criterion) {
    let mut group = c.benchmark_group("llm_edit_span");
    for lines in [1_000usize, 10_000, 100_000] {
        let old: String = (0..lines)
            .map(|i| format!("line {i} of the file\n"))
            .collect();
        let new = old.replacen(
            &format!("line {} of", lines / 2),
            &format!("LINE {} of", lines / 2),
            1,
        );
        group.bench_with_input(BenchmarkId::new("lines", lines), &lines, |b, _| {
            b.iter(|| lattice_agent::editor_access::differing_span(&old, &new))
        });
    }
    group.finish();
}

/// LL.18: the permission popup's tick asks every producer for the oldest
/// pending request, on the editor thread, on every tick. The usual answer is
/// "none", across however many chats are open; this is that walk.
fn bench_permission_scan(c: &mut Criterion) {
    use lattice_ai::conversation::ConversationStore;
    use lattice_ai::permission::PermissionSources;
    use std::sync::Arc;
    let mut group = c.benchmark_group("llm_permission_scan");
    for chats in [1usize, 10, 100] {
        let stores: Vec<ConversationStore> = (0..chats)
            .map(|_| ConversationStore::new(Arc::new(|_| {})))
            .collect();
        let sources = PermissionSources::new();
        sources.add(Arc::new(move || stores.clone()));
        group.bench_with_input(BenchmarkId::new("chats", chats), &chats, |b, _| {
            b.iter(|| black_box(sources.oldest_pending_where(|_| true).is_none()))
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    bench,
    bench_tool_history,
    bench_edit_span,
    bench_permission_scan
);
criterion_main!(benches);
