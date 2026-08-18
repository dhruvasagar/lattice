//! TC.3/TC.5 — the whole chain, in one test, with a native producer.
//!
//! Everything up to now proved one hop each: the seam crosses, the drain
//! registers, the resolver resolves, the worker builds. Nothing asserted that
//! opening a file and scrolling into a function actually produces a strip —
//! which is exactly the report that came back from running the editor.
//!
//! This boots a real `Editor`, registers a producer, drives the same
//! `run_tick_pending` pump production drives, and asserts the pane's published
//! sticky-context layer. A native stub stands in for the WASM plugin so the
//! test does not depend on a wasm32 toolchain; the WASM half is proven by
//! `lattice-plugin-loader`'s `treesitter_context_plugin.rs`.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use lattice_cells::context::ContextScope;
use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;
use lattice_mode::{
    AsyncContextSource, ContextFuture, ContextSourceRegistry, ContextSourceRegistryHandle,
};

#[derive(Debug)]
struct StubProducer {
    scopes: Vec<ContextScope>,
    calls: Arc<AtomicU64>,
}

impl AsyncContextSource for StubProducer {
    fn source_id(&self) -> u64 {
        1
    }
    fn produce(
        &self,
        _buffer: u64,
        _path: Option<std::path::PathBuf>,
        _lines: u32,
        _syntax: Option<Arc<dyn std::any::Any + Send + Sync>>,
    ) -> ContextFuture<'_> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let scopes = self.scopes.clone();
        Box::pin(async move { Ok(scopes) })
    }
}

fn registry(scopes: Vec<ContextScope>, calls: Arc<AtomicU64>) -> ContextSourceRegistryHandle {
    let mut r = ContextSourceRegistry::new();
    r.register(Arc::new(StubProducer { scopes, calls }));
    Arc::new(arc_swap::ArcSwap::from_pointee(r))
}

fn scope(start: u32, end: u32) -> ContextScope {
    ContextScope {
        scope_start: start,
        scope_end: end,
        header_start: start,
        header_end: start,
    }
}

/// Wait for the producer's write to land in the cache.
async fn wait_for_cache(editor: &Editor, buffer: lattice_core::BufferId) {
    use lattice_host::per_buffer_cache::PerBufferCacheExt;
    for _ in 0..200 {
        if editor
            .wasm_context
            .cache
            .get_for(buffer)
            .map(|c| !c.scopes.is_empty())
            .unwrap_or(false)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A 200-line document. An outer scope at 10..=180 and an inner at 100..=150 —
/// the shape of an `impl` containing a `fn`.
fn editor_with_producer(calls: Arc<AtomicU64>) -> Editor {
    let text: String = (0..200).map(|i| format!("line {i}\n")).collect();
    let mut editor = Editor::boot(CoreDocument::from_text(&text));
    editor.wasm_context = lattice_host::wasm_context::WasmContextState::with_registry(registry(
        vec![scope(10, 180), scope(100, 150)],
        calls,
    ));
    editor.viewport_height = 40;
    {
        let pane = editor.pane_tree.active_mut();
        pane.viewport_height = 40;
        pane.viewport_width = 100;
    }
    editor
}

/// The report that prompted this test: open a file, scroll into a function,
/// see nothing. Asserts the pane's PUBLISHED layer, which is what the renderer
/// paints — not just the intermediate line list.
#[tokio::test]
async fn scrolling_into_a_scope_publishes_a_strip_for_the_pane() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut editor = editor_with_producer(calls.clone());
    let buffer = editor.document_buffer_id;
    let pane_id = editor.pane_tree.active().id;

    // The pump production runs. It drives the producer off-thread.
    editor.run_tick_pending();
    wait_for_cache(&editor, buffer).await;
    assert!(calls.load(Ordering::Relaxed) >= 1, "the producer ran");

    // Now put the cursor deep inside both scopes with the view below both
    // headers — the state a user is in after scrolling into a function.
    editor.cursor.line = 120;
    editor.scroll = 110;
    {
        let pane = editor.pane_tree.active_mut();
        pane.cursor.line = 120;
        pane.scroll = 110;
    }

    // The host resolves per pane when it publishes pane inputs.
    let lines = editor.resolve_sticky_context_lines(buffer, 120, 110, 40);
    assert_eq!(
        &*lines,
        &[10, 100],
        "both headers have scrolled away, so both pin"
    );

    // And the layer the RENDERER reads must actually carry them. This is the
    // hop the earlier slices never asserted end to end.
    editor.publish_render_state();
    for _ in 0..100 {
        if !editor.sticky_context_for(pane_id).load().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let published = editor.sticky_context_for(pane_id).load();
    assert_eq!(
        published.rows.len(),
        2,
        "the pane's published strip carries both header rows — this is what \
         both renderers paint, and an empty layer here is the user-visible \
         'no context appears' report"
    );
    assert_eq!(published.rows[0].source_line, 10);
    assert_eq!(published.rows[1].source_line, 100);
    assert!(
        !published.rows[0].cells.is_empty(),
        "the row has real cells — an empty cell slice paints a blank line, \
         which looks like a layout bug rather than a missing feature"
    );
}

/// At the top of a file nothing has scrolled away, so there is nothing to pin.
/// Worth pinning as intended behaviour: "I opened the file and see no strip" is
/// correct, and only becomes a bug once you scroll.
#[tokio::test]
async fn at_the_top_of_a_file_there_is_no_strip() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut editor = editor_with_producer(calls);
    let buffer = editor.document_buffer_id;

    editor.run_tick_pending();
    wait_for_cache(&editor, buffer).await;

    let lines = editor.resolve_sticky_context_lines(buffer, 0, 0, 40);
    assert!(
        lines.is_empty(),
        "nothing has scrolled off at the top of a file"
    );
}

/// TC.8a: a `treesitter-context.*` option the plugin registered must reach the
/// resolver.
///
/// It did not. `resolve_sticky_context_lines` built `ContextOptions::default()`
/// and never read the registry, so every knob the plugin advertised —
/// `max-lines`, `trim-scope`, `multiline-threshold`, `max-viewport-fraction` —
/// was inert: `:set treesitter-context.max-lines=1` changed the help text and
/// nothing else. That is worse than the option not existing, because the
/// editor answers `:set treesitter-context.max-lines?` with the value it is
/// ignoring.
#[tokio::test]
async fn a_registered_context_option_reaches_the_resolver() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut editor = editor_with_producer(calls);
    let buffer = editor.document_buffer_id;

    editor.run_tick_pending();
    wait_for_cache(&editor, buffer).await;

    // Both scopes enclose line 120 and both headers have scrolled away.
    let lines = editor.resolve_sticky_context_lines(buffer, 120, 110, 40);
    assert_eq!(&*lines, &[10, 100], "unbounded: both pin");

    // Cap the strip at one row, the way `:set` does.
    editor
        .config
        .try_register(lattice_config::option::Option::<i64>::new(
            "treesitter-context.max-lines".to_owned(),
            1,
            "Cap.".to_owned(),
        ))
        .expect("fresh name");
    // The cache refreshes on the same pump that drives the producer, so the
    // option lands without the user pressing anything.
    editor.run_tick_pending();

    let lines = editor.resolve_sticky_context_lines(buffer, 120, 110, 40);
    assert_eq!(
        &*lines,
        &[100],
        "capped at one row, and `trim-scope=outer` (the default) drops the \
         OUTER scope — the innermost is the one you are in"
    );
}

/// TC.8b: `context.line-numbers` reaches the layer both renderers paint from.
///
/// The unit tests prove each painter formats a gutter correctly given a
/// `gutter_line`; this proves the option actually decides whether there IS
/// one. Without it the feature can be fully correct in both peers and still
/// inert, which is exactly the state TC.8 was deferred in.
#[tokio::test]
async fn the_line_numbers_option_reaches_the_published_layer() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut editor = editor_with_producer(calls);
    let buffer = editor.document_buffer_id;
    let pane_id = editor.pane_tree.active().id;

    editor.run_tick_pending();
    wait_for_cache(&editor, buffer).await;

    editor.cursor.line = 120;
    editor.scroll = 110;
    {
        let pane = editor.pane_tree.active_mut();
        pane.cursor.line = 120;
        pane.scroll = 110;
    }
    editor.publish_render_state();
    for _ in 0..100 {
        if !editor.sticky_context_for(pane_id).load().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        editor.sticky_context_for(pane_id).load().line_numbers,
        "on by default — the plugin registers `line-numbers` true, and the \
         host's fallback matches so an unloaded plugin agrees with a loaded one"
    );

    // Turn it off the way `:set` does.
    editor
        .config
        .try_register(lattice_config::option::Option::<bool>::new(
            "treesitter-context.line-numbers".to_owned(),
            false,
            "Off.".to_owned(),
        ))
        .expect("fresh name");
    editor.run_tick_pending();
    editor.publish_render_state();
    for _ in 0..100 {
        if !editor.sticky_context_for(pane_id).load().line_numbers {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let published = editor.sticky_context_for(pane_id).load();
    assert!(
        !published.line_numbers,
        "the toggle must reach the layer without the lines changing — the \
         worker's early-return compares the flag for exactly this reason"
    );
    assert_eq!(
        published.rows.len(),
        2,
        "and the rows themselves are untouched: this is a gutter option, not \
         a resolution one"
    );
}

/// TC.12: `context.anchor = topline` resolves against the pane's first visible
/// line instead of the cursor.
///
/// The option was specified in full (design fragment, "Anchor: cursor, and why
/// not topline") and registered from TC.5, and the resolver never saw it —
/// `resolve_sticky_context_lines` always passed the cursor.
///
/// The scenario is chosen so the two anchors genuinely DISAGREE: cursor 120 is
/// inside the inner scope (100..=150) while topline 160 is past its end. A
/// scenario where both agree would pass against the unfixed code, which is the
/// trap this whole verification pass exists to catch.
#[tokio::test]
async fn the_anchor_option_switches_what_the_strip_resolves_against() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut editor = editor_with_producer(calls);
    let buffer = editor.document_buffer_id;
    editor.run_tick_pending();
    wait_for_cache(&editor, buffer).await;

    let cursor = 120;
    let top = 160;

    let by_cursor = editor.resolve_sticky_context_lines(buffer, cursor, top, 40);
    assert_eq!(
        &*by_cursor,
        &[10, 100],
        "cursor anchor: the cursor is inside both scopes and both headers have \
         scrolled away"
    );

    editor
        .config
        .try_register(lattice_config::option::Option::<String>::new(
            "treesitter-context.anchor".to_owned(),
            "topline".to_owned(),
            "Anchor.".to_owned(),
        ))
        .expect("fresh name");
    editor.run_tick_pending();

    let by_topline = editor.resolve_sticky_context_lines(buffer, cursor, top, 40);
    assert_eq!(
        &*by_topline,
        &[10],
        "topline anchor: line 160 is past the inner scope's end, so only the \
         outer one encloses what the pane is showing — a different answer from \
         the cursor's, which is the point of the option"
    );
}

/// TC.12: `context.separator` puts a rule under the strip, as a REAL row.
///
/// It matters that it is a row rather than a glyph the renderer appends: the
/// scroll model reserves `len()` rows, so a rule outside the list would be one
/// row nothing reserved for, and the text beneath would sit a line too high.
#[tokio::test]
async fn the_separator_option_adds_a_reserved_rule_row() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut editor = editor_with_producer(calls);
    let buffer = editor.document_buffer_id;
    let pane_id = editor.pane_tree.active().id;
    editor.run_tick_pending();
    wait_for_cache(&editor, buffer).await;

    editor.cursor.line = 120;
    editor.scroll = 110;
    {
        let pane = editor.pane_tree.active_mut();
        pane.cursor.line = 120;
        pane.scroll = 110;
    }
    editor.publish_render_state();
    for _ in 0..100 {
        if !editor.sticky_context_for(pane_id).load().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let without = editor.sticky_context_for(pane_id).load();
    assert_eq!(without.rows.len(), 2, "two scopes, no rule");
    assert!(!without.has_separator);
    assert_eq!(without.innermost_scope(), Some(1));

    editor
        .config
        .try_register(lattice_config::option::Option::<String>::new(
            "treesitter-context.separator".to_owned(),
            "-".to_owned(),
            "Rule.".to_owned(),
        ))
        .expect("fresh name");
    editor.run_tick_pending();
    editor.publish_render_state();
    for _ in 0..100 {
        if editor.sticky_context_for(pane_id).load().has_separator {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let with = editor.sticky_context_for(pane_id).load();
    assert!(with.has_separator, "the rule landed");
    assert_eq!(
        with.rows.len(),
        3,
        "and it is a real row, so `len()` — which is what the scroll model \
         reserves — counts it"
    );
    assert_eq!(
        with.innermost_scope(),
        Some(1),
        "the innermost SCOPE is still row 1; the rule must not take the \
         `active` styling meant for the scope the cursor is in"
    );
    assert!(
        with.rows[2].cells.iter().all(|c| c.codepoint == '-' as u32),
        "the rule is the glyph, repeated"
    );
}

/// TC.5: `context.enabled = false` must empty the strip.
///
/// This is what `:context-toggle` flips, so it is the option with the most
/// direct user-visible meaning of the whole set. The loader turns it into a
/// MODE enable/disable, which controls the chord — but nothing consulted it on
/// the render path, so the strip carried on painting after the user turned the
/// feature off.
///
/// Publishing an EMPTY list rather than skipping the publish is deliberate:
/// the reservation is computed from the published length, so a skipped publish
/// would leave the rows reserved and the text pushed down with nothing in the
/// gap.
#[tokio::test]
async fn disabling_the_feature_empties_the_strip() {
    let calls = Arc::new(AtomicU64::new(0));
    let mut editor = editor_with_producer(calls);
    let buffer = editor.document_buffer_id;
    editor.run_tick_pending();
    wait_for_cache(&editor, buffer).await;

    assert_eq!(
        &*editor.resolve_sticky_context_lines(buffer, 120, 110, 40),
        &[10, 100],
        "precondition: enabled by default"
    );

    editor
        .config
        .try_register(lattice_config::option::Option::<bool>::new(
            "treesitter-context.enabled".to_owned(),
            false,
            "Off.".to_owned(),
        ))
        .expect("fresh name");
    editor.run_tick_pending();

    assert!(
        editor
            .resolve_sticky_context_lines(buffer, 120, 110, 40)
            .is_empty(),
        "turned off means no rows — and an empty published list, so the \
         reservation goes to zero and the text moves back up"
    );
}
