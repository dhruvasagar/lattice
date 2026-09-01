//! The view options for ONE pane, resolved against the buffer that pane shows.
//!
//! GPUI's peer of the TUI's `FrameView` option fields, and the answer to the
//! same question: a pane's gutter, cursorline, sign columns and indent guides
//! are properties of the buffer being painted, not of whichever buffer happens
//! to be focused.
//!
//! ## Why this exists as a type rather than four inline reads
//!
//! It did exist as four inline reads, in the middle of the per-pane render
//! method, and three of them resolved against `active_document.option_cache` —
//! the FOCUSED buffer. So a magit view sitting beside a focused file grew line
//! numbers its own mode turns off, and a file beside a focused magit view lost
//! them. One of the four carried a comment admitting it ("inactive panes
//! inherit the active value — the same pre-existing per-pane-option
//! limitation"), and a fourth (`cursorline`) had a special case for preview
//! panes only, because that was the one configuration where a pane's buffer was
//! *known* to differ from the active document. It is true of every unfocused
//! pane.
//!
//! `RenderState::resolved_option_for` (PI.4) is the renderer-agnostic seam
//! built for exactly this, and its own doc names the defect it was meant to
//! end: peers resolving options "instead of the TUI reading the live editor and
//! GPUI reading the active document's `option_cache`". The TUI half landed;
//! this is the other one.
//!
//! Gathering them into a struct is what makes the rule checkable — the reads
//! are now in one place with one test, instead of four places where the next
//! option added silently picks the wrong source.

use lattice_core::BufferId;
use lattice_host::render_state::RenderState;

/// Per-pane resolved view options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneOptions {
    /// `:set number` — whether to reserve line-number digits in the gutter.
    pub show_line_numbers: bool,
    /// `:set cursorline` / `current-line-highlight-mode`.
    pub cursorline: bool,
    /// `:set signcolumn` — whether the two sign cells are reserved.
    pub sign_column: bool,
    /// `ui.indent-guides.active` — whether the cursor's block is emphasised.
    pub indent_guides_active: bool,
}

impl PaneOptions {
    /// Resolve every option against `buffer_id` — the buffer the pane is
    /// showing, never the active document.
    ///
    /// A buffer with no published entry falls back to the global typed-option
    /// default inside `resolved_option_for`, so a transient publish gap renders
    /// as the user's configured default rather than as another buffer's value.
    pub fn for_pane(rs: &RenderState, buffer_id: BufferId) -> Self {
        Self {
            show_line_numbers: *rs.resolved_option_for::<lattice_config::Number>(buffer_id),
            cursorline: rs.current_line_highlight_for(buffer_id),
            sign_column: rs
                .resolved_option_for::<lattice_config::SignColumnOption>(buffer_id)
                .reserved(),
            indent_guides_active: *rs
                .resolved_option_for::<lattice_config::core_options::IndentGuidesActive>(buffer_id),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_config::ResolvedOptions;
    use lattice_host::render_state::ResolvedOptionsRenderState;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Two buffers, one of which turns `number` and `cursorline` off the way
    /// every magit mode does. Each must get its OWN answer.
    ///
    /// This is the regression guard for the reported bug: with the options read
    /// from `active_document.option_cache`, both panes got one answer, and
    /// which answer depended on which pane was focused.
    #[test]
    fn each_pane_resolves_options_against_its_own_buffer() {
        let gutterless = BufferId::next();
        let ordinary = BufferId::next();

        // Every option `for_pane` reads is populated for both buffers:
        // `resolved_option_for` falls back to the global registry when a
        // buffer has no entry, and a default `RenderState` has none
        // registered, so a partial fixture panics rather than exercising the
        // fallback.
        let mut off = ResolvedOptions::new();
        off.insert::<lattice_config::Number>(false);
        off.insert::<lattice_config::CursorLine>(false);
        off.insert::<lattice_config::SignColumnOption>(lattice_config::SignColumn::No);
        off.insert::<lattice_config::core_options::IndentGuidesActive>(false);
        let mut on = ResolvedOptions::new();
        on.insert::<lattice_config::Number>(true);
        on.insert::<lattice_config::CursorLine>(true);
        on.insert::<lattice_config::SignColumnOption>(lattice_config::SignColumn::Yes);
        on.insert::<lattice_config::core_options::IndentGuidesActive>(true);

        // The absent-buffer fallback is `resolved_option_for`'s own contract
        // and is left to that seam's tests rather than half-asserted here.
        let mut map: HashMap<BufferId, Arc<ResolvedOptions>> = HashMap::new();
        map.insert(gutterless, Arc::new(off));
        map.insert(ordinary, Arc::new(on));

        let rs = RenderState {
            resolved_opts: Arc::new(ResolvedOptionsRenderState { map: Arc::new(map) }),
            ..RenderState::default()
        };

        let a = PaneOptions::for_pane(&rs, gutterless);
        let b = PaneOptions::for_pane(&rs, ordinary);

        assert!(!a.show_line_numbers, "the buffer that turned `number` off");
        assert!(!a.cursorline);
        assert!(
            b.show_line_numbers,
            "…and its neighbour keeps its own, whichever pane is focused"
        );
        assert!(b.cursorline);
        assert!(
            !a.sign_column && b.sign_column,
            "signcolumn is per-pane too"
        );
        assert!(!a.indent_guides_active && b.indent_guides_active);
        assert_ne!(a, b, "two buffers, two answers — the whole point");
    }
}
