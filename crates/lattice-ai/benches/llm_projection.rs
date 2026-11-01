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
    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
