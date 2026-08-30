#![allow(clippy::unwrap_used, clippy::panic)]
//! OR.6 criterion bench for a find-node picker open.
//!
//! What a `:org-roam-find-node` costs at the moment the user presses the key,
//! against the reference corpus's 585 nodes. Three parts, measured separately
//! because they fail differently:
//!
//!   * `store_get_nodes` — the one `get` of the ~90 KB `nodes` blob. This is
//!     the number that justifies §4.2 keeping the blob at all: find-node is one
//!     `get`, not 585.
//!   * `deserialize_nodes` — turning those bytes into records. The guest pays
//!     this, once per open.
//!   * `rank_first_frame` — the picker's own fuzzy match + rank over 585
//!     candidates, which is what happens on the FIRST keystroke of a query and
//!     on every one after it.
//!
//! **The third is the one with a budget.** The first two happen once per open,
//! where a millisecond is invisible; ranking happens per keystroke, where it is
//! not. Splitting them means a regression in ranking cannot hide behind an
//! open-time cost that nobody feels.
//!
//! The store and the ranker are both measured natively — the WASM crossing is
//! `boundary.rs`'s subject. A guest-side matcher would put a crossing on every
//! keystroke, which is the thing `roam_find.rs` exists to avoid; this bench is
//! the number that says what was avoided.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use lattice_completion::{
    CandidateData, CandidateKind, CompletionPipeline, OrderlessDisplayMatcher, RawCandidate,
};
use lattice_plugin_host::plugin_store::PluginStore;
use serde::{Deserialize, Serialize};

/// The node record, as the guest encodes it. Mirrored rather than imported:
/// the plugin is a `cdylib` compiled for wasm, so a bench that used the guest's
/// own type would be measuring the encoder against itself.
#[derive(Clone, Serialize, Deserialize)]
struct Node {
    id: String,
    title: String,
    aliases: Vec<String>,
    tags: Vec<String>,
    refs: Vec<String>,
    file: String,
    line: u32,
    level: u32,
}

/// The reference corpus's shape: 585 nodes, 71 of them with an alias, most with
/// a tag or two, titles a handful of words long.
fn corpus() -> Vec<Node> {
    (0..585)
        .map(|i| Node {
            id: format!("{i:08X}-1111-2222-3333-444455556666"),
            title: format!("Note {i} about something reasonably wordy"),
            aliases: if i % 8 == 0 {
                vec![format!("Alias for {i}")]
            } else {
                Vec::new()
            },
            tags: vec!["topic".into(), format!("t{}", i % 12)],
            refs: Vec::new(),
            file: format!("/roam/2025060310{i:04}-note_{i}.org"),
            line: if i % 5 == 0 { 0 } else { (i % 40) as u32 },
            level: u32::from(i % 5 != 0),
        })
        .collect()
}

/// The candidate rows `roam_find::init` builds — title as text, alias folded
/// into the display so it is matchable.
fn candidates(nodes: &[Node]) -> Vec<RawCandidate> {
    nodes
        .iter()
        .map(|n| {
            let display = if n.aliases.is_empty() {
                n.title.clone()
            } else {
                format!("{}  ({})", n.title, n.aliases.join(", "))
            };
            let mut c = RawCandidate::plain(n.title.clone(), CandidateKind::Plain);
            c.display = display;
            c.data = CandidateData::Plain;
            c
        })
        .collect()
}

fn bench_find_open(c: &mut Criterion) {
    let dir = tempfile::tempdir().unwrap();
    let nodes = corpus();
    let blob = rmp_serde::to_vec(&nodes).unwrap();
    eprintln!("nodes blob: {} bytes for {} nodes", blob.len(), nodes.len());

    let mut store = PluginStore::open(dir.path());
    store.put("nodes", blob.clone()).unwrap();

    c.bench_function("roam_find/store_get_nodes", |b| {
        b.iter(|| black_box(store.get(black_box("nodes"))));
    });

    c.bench_function("roam_find/deserialize_nodes", |b| {
        b.iter(|| {
            let decoded: Vec<Node> = rmp_serde::from_slice(black_box(&blob)).unwrap();
            black_box(decoded.len())
        });
    });

    // The per-keystroke number. A query that matches a middling number of rows
    // is the honest case: an empty query short-circuits the matcher and a query
    // matching nothing exits early on most candidates.
    let raw = candidates(&nodes);
    let pipeline = CompletionPipeline {
        generators: Vec::new(),
        matcher: std::sync::Arc::new(OrderlessDisplayMatcher),
        rankers: Vec::new(),
        annotators: Vec::new(),
    };
    c.bench_function("roam_find/rank_first_frame_585", |b| {
        b.iter(|| black_box(pipeline.match_and_rank(black_box("note ab"), &raw).len()));
    });
}

criterion_group!(benches, bench_find_open);
criterion_main!(benches);
