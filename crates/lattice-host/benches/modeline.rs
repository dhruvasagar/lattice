#![allow(clippy::unwrap_used, clippy::panic)]
//! ML.1a-render: modeline build-cost bench.
//!
//! The renderer lays out the modeline once per pane per frame by
//! resolving the registered elements into per-zone content. This bench
//! proves that build is **O(elements)**: it times a full "resolve all
//! three zones" pass against registries of increasing element count.
//! The cost scales linearly (a `zone_ordered` sort + a per-element
//! resolve) and is independent of document size — built-in content is a
//! pure read off the published `RenderState`, never proportional to the
//! buffer (paramount #1).
//!
//! Run:
//!
//!   cargo bench -p lattice-host --bench modeline
//!
//! The per-N rows should grow linearly in the element count `N`.

use criterion::{BenchmarkId, Criterion, black_box, criterion_group, criterion_main};
use lattice_core::Document;
use lattice_host::editor::Editor;
use lattice_host::modeline;
use lattice_mode::{ElementContent, ElementId, ModelineElement, ModelineKey, ModelineRole, Zone};

/// Boot an editor (its four `core.*` built-ins registered) and push
/// `extra` synthetic PaneLocal elements spread across the three zones,
/// each with non-empty content so it renders.
fn editor_with_elements(extra: usize) -> Editor {
    let editor = Editor::boot(Document::empty());
    // Push content keyed to the active pane's buffer so the bench mirrors
    // the renderer's per-pane `resolve` (ML.3 per-buffer content keying).
    let buffer = editor.pane_tree.active().buffer_id;
    for i in 0..extra {
        let id = ElementId::new(format!("plugin.e{i}"));
        let zone = match i % 3 {
            0 => Zone::Left,
            1 => Zone::Center,
            _ => Zone::Right,
        };
        editor
            .modeline
            .register(ModelineElement::new(id.clone(), zone, 100 + i as i32));
        editor.modeline.update(
            ModelineKey::Buffer(buffer),
            id,
            ElementContent::text(format!("e{i}"), ModelineRole::new(modeline::ROLE_MODE_ITEM)),
        );
    }
    editor
}

fn bench_modeline_build(c: &mut Criterion) {
    let mut group = c.benchmark_group("modeline_build");
    for &n in &[0usize, 8, 32, 128] {
        let mut editor = editor_with_elements(n);
        let rs = editor.build_render_state();
        let pane = *editor.pane_tree.active();
        group.bench_with_input(BenchmarkId::from_parameter(n), &n, |b, _| {
            b.iter(|| {
                let snap = &rs.modeline_elements;
                // Mirror the renderer's per-frame zone resolution exactly:
                // ML.5 routes membership/order through `resolve_layout`
                // (config read + claim-set + per-zone descriptor list),
                // then resolves each descriptor's content. Still
                // O(elements), independent of document size.
                let layout = modeline::resolve_layout(&snap.registry, &rs.options.config);
                for els in [&layout.left, &layout.center, &layout.right] {
                    for el in els {
                        let id = el.id.as_str();
                        let content = if id.starts_with("core.") {
                            modeline::resolve_builtin_content(id, &pane, true, &rs, None)
                        } else {
                            snap.resolve(el, pane.buffer_id)
                                .cloned()
                                .unwrap_or_default()
                        };
                        black_box(content.plain());
                    }
                }
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_modeline_build);
criterion_main!(benches);
