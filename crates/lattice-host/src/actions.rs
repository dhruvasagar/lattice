//! App-side action registrations -- the `CommandKind::Action`
//! peers of the grammar's built-in motions / operators / text-
//! objects (`lattice_grammar::builtins`) and built-in ex-commands
//! (`lattice_grammar::ex_commands`).
//!
//! See `docs/dev/notes/8i-approach.md` for the slice 8.i plan. Each action
//! registered here returns `Effect::AppAction(AppEffect::Foo)`
//! from its `apply` closure; the App's `apply_app_effect` then
//! routes the `AppEffect` to the historical handler. Once slice
//! 8.i.4 retires the legacy `Action` enum, the bodies move
//! directly into `apply_app_effect` and this layer becomes the
//! sole producer.
//!
//! New AppEffect variants land here as a single line per variant
//! plus a one-line ID field on [`ActionIds`]; the actual
//! per-mode chord bindings live in `keymap_normal.rs` (and
//! sibling per-mode modules) and consume [`ActionIds`] alongside
//! `Builtins`.

use lattice_grammar::AppEffect;
use lattice_grammar::CommandRegistry;
use lattice_grammar::HScroll;
use lattice_grammar::ModalState;
use lattice_grammar::PaneDirection;
use lattice_grammar::Register;
use lattice_grammar::ScrollPos;
use lattice_grammar::SearchDirection;
use lattice_grammar::ViewportPos;
use lattice_grammar::VisualKind;
use lattice_grammar::args::Args;
use lattice_grammar::builtins::Builtins;
use lattice_grammar::effect::Effect;
use lattice_grammar::registry::ActionSpec;
use lattice_grammar::registry::OperatorId;
use lattice_protocol::ids::CommandId;

/// Strongly-typed handles to every App-side action registered
/// in the global [`CommandRegistry`]. Mirrors the shape of
/// `lattice_grammar::builtins::Builtins`: each field is the
/// `CommandId` produced by [`CommandRegistry::register_action`]
/// at startup. The App stores this struct; per-mode keymap
/// modules consume it to build typed `CommandInvocation`s for
/// chord bindings.
#[derive(Debug, Clone, Copy, Default)]
pub struct ActionIds {
    pub match_bracket: CommandId,
    pub toggle_case_at_cursor: CommandId,
    pub open_line_below: CommandId,
    pub open_line_above: CommandId,
    pub lsp_hover_request: CommandId,
    pub search_next: CommandId,
    pub search_previous: CommandId,
    pub jump_history_back: CommandId,
    pub jump_history_forward: CommandId,
    pub walk_mark_history_back: CommandId,
    pub walk_mark_history_forward: CommandId,
    pub tag_stack_pop: CommandId,
    pub open_fold_at_cursor: CommandId,
    pub close_fold_at_cursor: CommandId,
    pub toggle_fold_at_cursor: CommandId,
    pub open_all_folds: CommandId,
    pub close_all_folds: CommandId,
    pub cycle_fold_at_cursor: CommandId,
    pub cycle_folds_global: CommandId,
    pub goto_parent_fold: CommandId,
    pub delete_fold_at_cursor: CommandId,
    pub goto_next_fold: CommandId,
    pub goto_prev_fold: CommandId,
    pub toggle_fold_enable: CommandId,
    pub undo: CommandId,
    pub redo: CommandId,
    pub repeat_last_change: CommandId,
    pub page_down: CommandId,
    pub page_up: CommandId,
    pub scroll_line_up: CommandId,
    pub scroll_line_down: CommandId,
    pub redraw_screen: CommandId,
    pub open_command_picker: CommandId,
    pub enter_command_line: CommandId,
    pub oil_navigate_up: CommandId,
    pub reselect_last_visual: CommandId,
    pub paste_after: CommandId,
    pub paste_before: CommandId,
    pub lsp_definition_request: CommandId,
    pub lsp_declaration_request: CommandId,
    pub lsp_type_definition_request: CommandId,
    pub lsp_implementation_request: CommandId,
    pub lsp_references_request: CommandId,
    pub lsp_follow_link_at_cursor: CommandId,
    pub enter_append: CommandId,
    pub enter_insert_first_non_blank: CommandId,
    pub enter_append_end_of_line: CommandId,
    pub display_line_down: CommandId,
    pub display_line_up: CommandId,
    pub display_line_start: CommandId,
    pub display_line_end: CommandId,
    pub create_fold_from_visual: CommandId,
    pub delete_char_backward: CommandId,
    /// Insert-mode line editing (readline/vim). One `CommandId` per chord;
    /// all resolve to `AppEffect::InsertLineEdit(kind)`.
    pub insert_cursor_line_start: CommandId,
    pub insert_cursor_line_end: CommandId,
    pub insert_cursor_char_left: CommandId,
    pub insert_cursor_char_right: CommandId,
    pub insert_delete_word_backward: CommandId,
    pub insert_delete_to_line_start: CommandId,
    pub insert_kill_to_line_end: CommandId,
    pub insert_indent_line: CommandId,
    pub insert_dedent_line: CommandId,
    pub completion_trigger: CommandId,
    pub snippet_expand: CommandId,
    /// L4b: `gl` in lsp-diagnostics-mode. Command-name registration
    /// only — the handler body is `lsp-diagnostics-mode`'s mode-owned
    /// `ActionHandlerRegistry` closure (emits `ShowDiagnosticsPopup`);
    /// the `apply` below is a dead `Effect::None`, like `snippet_expand`.
    pub lsp_diagnostic_popup: CommandId,
    pub exit_visual: CommandId,
    pub swap_visual_ends: CommandId,
    pub replace_undo_last: CommandId,
    pub enter_mode_insert: CommandId,
    pub enter_mode_normal: CommandId,
    pub enter_mode_replace: CommandId,
    pub enter_visual_charwise: CommandId,
    pub enter_visual_linewise: CommandId,
    pub enter_visual_blockwise: CommandId,
    /// SN.3d Select-mode entry chords `gh` / `gH` / `g<C-h>`.
    pub enter_select_charwise: CommandId,
    pub enter_select_linewise: CommandId,
    pub enter_select_blockwise: CommandId,
    pub enter_search_forward: CommandId,
    pub enter_search_backward: CommandId,
    pub search_word_under_cursor_forward: CommandId,
    pub search_word_under_cursor_backward: CommandId,
    pub jump_viewport_top: CommandId,
    pub jump_viewport_middle: CommandId,
    pub jump_viewport_bottom: CommandId,
    pub scroll_cursor_to_top: CommandId,
    pub scroll_cursor_to_center: CommandId,
    pub scroll_cursor_to_bottom: CommandId,
    pub h_scroll_right: CommandId,
    pub h_scroll_left: CommandId,
    pub h_scroll_half_right: CommandId,
    pub h_scroll_half_left: CommandId,
    pub h_scroll_cursor_left_edge: CommandId,
    pub h_scroll_cursor_right_edge: CommandId,
    pub join_lines_with_space: CommandId,
    pub join_lines_bare: CommandId,
    pub find_repeat_forward: CommandId,
    pub find_repeat_reverse: CommandId,
    pub insert_newline: CommandId,
    pub insert_tab: CommandId,
    pub overwrite_char: CommandId,
    pub set_mark: CommandId,
    pub jump_to_mark_line: CommandId,
    pub jump_to_mark_exact: CommandId,
    pub select_register: CommandId,
    pub start_macro_record: CommandId,
    pub play_macro: CommandId,
    /// Slice 8.i.4.c: operator-prefix arms (`d`, `c`, `y`, `>`,
    /// `<`, `gU`, `gu`, `g~`). Each chord binds a typed
    /// `CommandInvocation` whose `ActionSpec` returns
    /// `AppEffect::AbsorbOperatorPrefix(op_id)`. The App
    /// handler latches `pending_count` -> `op_count` and
    /// pushes the operator's chord prefix into
    /// `App::partial_chord`. Replaces the legacy
    /// `Action::SetPending(Pending::AfterOperator(_))` flow.
    pub absorb_operator_delete: CommandId,
    pub absorb_operator_change: CommandId,
    pub absorb_operator_yank: CommandId,
    pub absorb_operator_indent_right: CommandId,
    pub absorb_operator_indent_left: CommandId,
    pub absorb_operator_upper: CommandId,
    pub absorb_operator_lower: CommandId,
    pub absorb_operator_toggle_case: CommandId,
    pub split_pane_horizontal: CommandId,
    pub split_pane_vertical: CommandId,
    pub close_pane: CommandId,
    /// `<C-w>o` / `:only` / emacs `C-x 1` -- close every pane except
    /// the active one. S3b (2026-06-22).
    pub only_pane: CommandId,
    pub navigate_pane_left: CommandId,
    pub navigate_pane_down: CommandId,
    pub navigate_pane_up: CommandId,
    pub navigate_pane_right: CommandId,
    pub next_pane: CommandId,
    pub prev_pane: CommandId,
    /// Issue #28 (2026-05-22): split-ratio adjustment IDs.
    pub equalize_panes: CommandId,
    pub grow_pane_height: CommandId,
    pub shrink_pane_height: CommandId,
    pub grow_pane_width: CommandId,
    pub shrink_pane_width: CommandId,
    /// Issue #32 (2026-05-22): picker open-target override
    /// CommandIds. `<C-s>` / `<C-v>` / `<C-t>` on picker
    /// overlays bind to these.
    pub picker_accept_in_split: CommandId,
    pub picker_accept_in_vsplit: CommandId,
    pub picker_accept_in_tab: CommandId,
    /// Issue #29 (2026-05-22): tab management IDs.
    pub next_tab: CommandId,
    pub prev_tab: CommandId,
    pub new_tab: CommandId,
    pub close_tab: CommandId,
    /// Slice 3 (2026-05-22): tabonly + tabmove. `tabmove`'s
    /// numeric target is carried by the dispatched
    /// `Action::MoveTab(u32)` payload; the CommandId here just
    /// names the action for keymap binding + plugin lookup.
    pub only_tab: CommandId,
    pub move_tab: CommandId,
    /// T4 (2026-05-25): `<C-w>T` — move the active pane to a
    /// fresh tab. Dispatched as `Action::MovePaneToNewTab`
    /// (handler in `Editor::do_move_pane_to_new_tab`).
    pub move_pane_to_new_tab: CommandId,
    /// Slice 8.i.4.e: completion-popup overlay actions
    /// (registered into a minor-mode layer pushed when the
    /// popup opens; popped on close). Each ID is the typed
    /// `CommandInvocation` peer of the legacy
    /// `Action::Completion*` variants.
    pub completion_next: CommandId,
    pub completion_prev: CommandId,
    pub completion_accept: CommandId,
    pub completion_cancel: CommandId,
    pub completion_cancel_and_exit_insert: CommandId,
    pub completion_toggle_docs: CommandId,
    pub completion_docs_scroll_down: CommandId,
    pub completion_docs_scroll_up: CommandId,
    pub completion_accept_then_insert: CommandId,
    /// CSM.K2: restrict the popup to a single completion source.
    /// Args::String(source_id), e.g. `"gen:buffer-words"`. Bound
    /// to popup-mode filter chords (`<C-b>`, `<C-o>`, `<C-f>`,
    /// `<C-t>`, ...).
    pub completion_filter_to_source: CommandId,
    /// CSM.K2: clear the active source filter (`<C-Space>`).
    pub completion_filter_clear: CommandId,
    /// Slice 8.i.4.e: active-snippet overlay actions
    /// (registered into a minor-mode layer pushed when a
    /// snippet activates; popped on exit).
    // CR.6 (2026-06-24): `diff_get`/`diff_put` ActionIds removed — the diff
    // actions (incl. `action:diff-get`/`-put`) are registered by
    // `lattice_diff::install()` now, not host `actions.rs`.
    pub snippet_next_placeholder: CommandId,
    pub snippet_prev_placeholder: CommandId,
    pub snippet_leave: CommandId,
    /// M.6.1 (2026-06-01): `<CR>` chord under
    /// `MinorMode(project-search-multibuffer-mode)`. Jump to
    /// source file/row of the excerpt under cursor.
    pub search_jump_to_source: CommandId,
    /// M.6.1 (2026-06-01): `gr` chord under
    /// `MinorMode(project-search-multibuffer-mode)`. Re-run the
    /// scan with the view's current query.
    pub search_refresh: CommandId,
}

/// Register every App-side action into `registry` and return
/// the resulting [`ActionIds`]. Called once at App startup,
/// after `lattice_grammar::builtins::populate` and
/// `lattice_grammar::ex_commands::populate`.
pub fn populate(registry: &mut CommandRegistry, builtins: &Builtins) -> ActionIds {
    // CR.6 (2026-06-24): the diff conflict-resolution action shells moved to
    // `lattice_diff::install()` (the "modes register commands" pattern) —
    // they no longer register here.
    ActionIds {
        match_bracket: register_simple(
            registry,
            "action:match-bracket",
            "Vim's `%`: jump to the matching bracket.",
            AppEffect::MatchBracket,
        ),
        toggle_case_at_cursor: register_simple(
            registry,
            "action:toggle-case-at-cursor",
            "Vim's `~`: toggle the case of the char at the cursor.",
            AppEffect::ToggleCaseAtCursor,
        ),
        open_line_below: register_simple(
            registry,
            "action:open-line-below",
            "Vim's `o`: open a new line below and enter Insert.",
            AppEffect::OpenLineBelow,
        ),
        open_line_above: register_simple(
            registry,
            "action:open-line-above",
            "Vim's `O`: open a new line above and enter Insert.",
            AppEffect::OpenLineAbove,
        ),
        // L7: `K` is mode-owned. The `CommandId` still resolves (the
        // chord binding + the `lsp-mode` handler registration key on it),
        // but the `apply` is a dead `Effect::None` — the
        // `ActionHandlerRegistry` closure intercepts first and emits
        // `Effect::Lsp(LspRequest::Hover)`. Same shape as `snippet_expand`
        // / `lsp_diagnostic_popup`.
        lsp_hover_request: registry.register_action(
            "action:lsp-hover",
            "`K`: send `textDocument/hover` to attached LSP servers \
             (mode-owned; `lsp-mode`'s handler emits `Effect::Lsp(LspRequest::Hover)`).",
            ActionSpec {
                apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        ),
        search_next: register_simple(
            registry,
            "action:search-next",
            "Vim's `n`: re-run the last search forward.",
            AppEffect::SearchNext,
        ),
        search_previous: register_simple(
            registry,
            "action:search-previous",
            "Vim's `N`: re-run the last search in the reverse direction.",
            AppEffect::SearchPrevious,
        ),
        jump_history_back: register_simple(
            registry,
            "action:jump-history-back",
            "Vim's `<C-o>`: step backward through the position history.",
            AppEffect::JumpHistoryBack,
        ),
        jump_history_forward: register_simple(
            registry,
            "action:jump-history-forward",
            "Vim's `<C-i>`: step forward through the position history.",
            AppEffect::JumpHistoryForward,
        ),
        walk_mark_history_back: register_simple(
            registry,
            "action:walk-mark-history-back",
            "Vim's `g;`: walk backward through the mark history.",
            AppEffect::WalkMarkHistoryBack,
        ),
        walk_mark_history_forward: register_simple(
            registry,
            "action:walk-mark-history-forward",
            "Vim's `g,`: walk forward through the mark history.",
            AppEffect::WalkMarkHistoryForward,
        ),
        tag_stack_pop: register_simple(
            registry,
            "action:tag-stack-pop",
            "Vim's `<C-t>`: pop the tag stack and jump back to the prior origin.",
            AppEffect::TagStackPop,
        ),
        open_fold_at_cursor: register_simple(
            registry,
            "action:open-fold-at-cursor",
            "Vim's `zo`: open the fold containing the cursor.",
            AppEffect::OpenFoldAtCursor,
        ),
        close_fold_at_cursor: register_simple(
            registry,
            "action:close-fold-at-cursor",
            "Vim's `zc`: close the fold containing the cursor.",
            AppEffect::CloseFoldAtCursor,
        ),
        toggle_fold_at_cursor: register_simple(
            registry,
            "action:toggle-fold-at-cursor",
            "Vim's `za`: toggle the fold containing the cursor.",
            AppEffect::ToggleFoldAtCursor,
        ),
        open_all_folds: register_simple(
            registry,
            "action:open-all-folds",
            "Vim's `zR`: open every fold in the buffer.",
            AppEffect::OpenAllFolds,
        ),
        close_all_folds: register_simple(
            registry,
            "action:close-all-folds",
            "Vim's `zM`: close every fold in the buffer.",
            AppEffect::CloseAllFolds,
        ),
        cycle_fold_at_cursor: register_simple(
            registry,
            "action:fold-cycle",
            "org-cycle (`z<Space>`): cycle the fold under the cursor through \
             FOLDED → CHILDREN → SUBTREE.",
            AppEffect::CycleFoldAtCursor,
        ),
        cycle_folds_global: register_simple(
            registry,
            "action:fold-cycle-global",
            "org-cycle (`z<Tab>`): cycle the whole buffer through \
             OVERVIEW → CONTENTS → SHOW-ALL.",
            AppEffect::CycleFoldsGlobal,
        ),
        goto_parent_fold: register_simple(
            registry,
            "action:fold-goto-parent",
            "Move the cursor to the parent heading, one level up the fold \
             hierarchy (`zp`).",
            AppEffect::GotoParentFold,
        ),
        delete_fold_at_cursor: register_simple(
            registry,
            "action:delete-fold-at-cursor",
            "Vim's `zd`: delete the fold containing the cursor.",
            AppEffect::DeleteFoldAtCursor,
        ),
        goto_next_fold: register_simple(
            registry,
            "action:goto-next-fold",
            "Vim's `zj`: move cursor to the start of the next fold.",
            AppEffect::GotoNextFold,
        ),
        goto_prev_fold: register_simple(
            registry,
            "action:goto-prev-fold",
            "Vim's `zk`: move cursor to the end of the previous fold.",
            AppEffect::GotoPrevFold,
        ),
        toggle_fold_enable: register_simple(
            registry,
            "action:toggle-fold-enable",
            "Vim's `zi`: toggle the `foldenable` option.",
            AppEffect::ToggleFoldEnable,
        ),
        undo: register_simple(
            registry,
            "action:undo",
            "Vim's `u`: undo the last buffer change.",
            AppEffect::Undo,
        ),
        redo: register_simple(
            registry,
            "action:redo",
            "Vim's `<C-r>`: redo the last undone change.",
            AppEffect::Redo,
        ),
        repeat_last_change: register_simple(
            registry,
            "action:repeat-last-change",
            "Vim's `.`: repeat the last change.",
            AppEffect::RepeatLastChange,
        ),
        page_down: register_simple(
            registry,
            "action:page-down",
            "Vim's `<C-f>`: scroll the viewport down one page.",
            AppEffect::PageDown,
        ),
        page_up: register_simple(
            registry,
            "action:page-up",
            "Vim's `<C-b>`: scroll the viewport up one page.",
            AppEffect::PageUp,
        ),
        scroll_line_up: register_simple(
            registry,
            "action:scroll-line-up",
            "Vim's `<C-y>`: scroll viewport up one line.",
            AppEffect::ScrollLineUp,
        ),
        scroll_line_down: register_simple(
            registry,
            "action:scroll-line-down",
            "Vim's `<C-e>`: scroll viewport down one line.",
            AppEffect::ScrollLineDown,
        ),
        redraw_screen: register_simple(
            registry,
            "action:redraw-screen",
            "Vim's `<C-l>`: force a full screen redraw.",
            AppEffect::RedrawScreen,
        ),
        open_command_picker: register_simple(
            registry,
            "action:open-command-picker",
            "Vim's `:` / Emacs' `M-x`: open the command picker.",
            AppEffect::OpenCommandPicker,
        ),
        enter_command_line: register_simple(
            registry,
            "action:enter-command-line",
            "Vim's `:`: enter the command-line minibuffer.",
            AppEffect::EnterCommandLine,
        ),
        oil_navigate_up: register_simple(
            registry,
            "action:oil-navigate-up",
            "Lattice's `-`: open / step up in the oil-style directory view.",
            AppEffect::OilNavigateUp,
        ),
        reselect_last_visual: register_simple(
            registry,
            "action:reselect-last-visual",
            "Vim's `gv`: reselect the last Visual selection.",
            AppEffect::ReselectLastVisual,
        ),
        paste_after: register_simple(
            registry,
            "action:paste-after",
            "Vim's `p`: paste the unnamed register's contents after the cursor.",
            AppEffect::PasteAfter,
        ),
        paste_before: register_simple(
            registry,
            "action:paste-before",
            "Vim's `P`: paste the unnamed register's contents before the cursor.",
            AppEffect::PasteBefore,
        ),
        // L7: the 6 nav chords are mode-owned. Each `CommandId` still
        // resolves (chord binding + `lsp-mode` handler registration key on
        // it) but carries a dead `Effect::None` apply — the
        // `ActionHandlerRegistry` closure intercepts first and emits
        // `Effect::Lsp(LspRequest::…)`. Same shape as `snippet_expand`.
        lsp_definition_request: registry.register_action(
            "action:lsp-definition",
            "`gd`: send `textDocument/definition` to attached LSP servers \
             (mode-owned; emits `Effect::Lsp(LspRequest::Definition)`).",
            ActionSpec {
                apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        ),
        lsp_declaration_request: registry.register_action(
            "action:lsp-declaration",
            "`gD`: send `textDocument/declaration` to attached LSP servers \
             (mode-owned; emits `Effect::Lsp(LspRequest::Declaration)`).",
            ActionSpec {
                apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        ),
        lsp_type_definition_request: registry.register_action(
            "action:lsp-type-definition",
            "`gy`: send `textDocument/typeDefinition` to attached LSP servers \
             (mode-owned; emits `Effect::Lsp(LspRequest::TypeDefinition)`).",
            ActionSpec {
                apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        ),
        lsp_implementation_request: registry.register_action(
            "action:lsp-implementation",
            "`gI`: send `textDocument/implementation` to attached LSP servers \
             (mode-owned; emits `Effect::Lsp(LspRequest::Implementation)`).",
            ActionSpec {
                apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        ),
        lsp_references_request: registry.register_action(
            "action:lsp-references",
            "`gr`: send `textDocument/references` to attached LSP servers \
             (mode-owned; emits `Effect::Lsp(LspRequest::References)`).",
            ActionSpec {
                apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        ),
        lsp_follow_link_at_cursor: registry.register_action(
            "action:lsp-follow-link",
            "`gx`: follow the `textDocument/documentLink` covering the cursor \
             (mode-owned; emits `Effect::Lsp(LspRequest::FollowLink)`).",
            ActionSpec {
                apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        ),
        enter_append: register_simple(
            registry,
            "action:enter-append",
            "Vim's `a`: move right one byte and enter Insert.",
            AppEffect::EnterAppend,
        ),
        insert_cursor_line_start: register_simple(
            registry,
            "action:insert-cursor-line-start",
            "Insert `<C-a>`: move the caret to the start of the line (readline).",
            AppEffect::InsertLineEdit(lattice_grammar::InsertLineEdit::CursorLineStart),
        ),
        insert_cursor_line_end: register_simple(
            registry,
            "action:insert-cursor-line-end",
            "Insert `<C-e>`: move the caret to the end of the line (readline).",
            AppEffect::InsertLineEdit(lattice_grammar::InsertLineEdit::CursorLineEnd),
        ),
        insert_cursor_char_left: register_simple(
            registry,
            "action:insert-cursor-char-left",
            "Insert `<C-b>`: move the caret one character left (readline).",
            AppEffect::InsertLineEdit(lattice_grammar::InsertLineEdit::CursorCharLeft),
        ),
        insert_cursor_char_right: register_simple(
            registry,
            "action:insert-cursor-char-right",
            "Insert `<C-f>`: move the caret one character right (readline).",
            AppEffect::InsertLineEdit(lattice_grammar::InsertLineEdit::CursorCharRight),
        ),
        insert_delete_word_backward: register_simple(
            registry,
            "action:insert-delete-word-backward",
            "Insert `<C-w>`: delete the word before the caret (readline/vim).",
            AppEffect::InsertLineEdit(lattice_grammar::InsertLineEdit::DeleteWordBackward),
        ),
        insert_delete_to_line_start: register_simple(
            registry,
            "action:insert-delete-to-line-start",
            "Insert `<C-u>`: delete from the line start to the caret (readline/vim).",
            AppEffect::InsertLineEdit(lattice_grammar::InsertLineEdit::DeleteToLineStart),
        ),
        insert_kill_to_line_end: register_simple(
            registry,
            "action:insert-kill-to-line-end",
            "Insert `<C-k>`: delete from the caret to the line end (readline).",
            AppEffect::InsertLineEdit(lattice_grammar::InsertLineEdit::KillToLineEnd),
        ),
        insert_indent_line: register_simple(
            registry,
            "action:insert-indent-line",
            "Insert `<C-t>`: indent the current line by one shiftwidth (vim).",
            AppEffect::InsertLineEdit(lattice_grammar::InsertLineEdit::IndentLine),
        ),
        insert_dedent_line: register_simple(
            registry,
            "action:insert-dedent-line",
            "Insert `<C-d>`: dedent the current line by one shiftwidth (vim).",
            AppEffect::InsertLineEdit(lattice_grammar::InsertLineEdit::DedentLine),
        ),
        enter_insert_first_non_blank: register_simple(
            registry,
            "action:enter-insert-first-non-blank",
            "Vim's `I`: move to first non-blank of line and enter Insert.",
            AppEffect::EnterInsertFirstNonBlank,
        ),
        enter_append_end_of_line: register_simple(
            registry,
            "action:enter-append-end-of-line",
            "Vim's `A`: move to end of line and enter Insert.",
            AppEffect::EnterAppendEndOfLine,
        ),
        display_line_down: register_simple(
            registry,
            "action:display-line-down",
            "Vim's `gj`: move down one display line (wrap segment).",
            AppEffect::DisplayLineDown,
        ),
        display_line_up: register_simple(
            registry,
            "action:display-line-up",
            "Vim's `gk`: move up one display line (wrap segment).",
            AppEffect::DisplayLineUp,
        ),
        display_line_start: register_simple(
            registry,
            "action:display-line-start",
            "Vim's `g0`: move to the start of the current display segment.",
            AppEffect::DisplayLineStart,
        ),
        display_line_end: register_simple(
            registry,
            "action:display-line-end",
            "Vim's `g$`: move to the end of the current display segment.",
            AppEffect::DisplayLineEnd,
        ),
        create_fold_from_visual: register_simple(
            registry,
            "action:create-fold-from-visual",
            "Vim's `zf`: create a fold from the most recent Visual selection.",
            AppEffect::CreateFoldFromVisual,
        ),
        delete_char_backward: register_simple(
            registry,
            "action:delete-char-backward",
            "Insert mode's `<BS>`: delete the byte before the cursor.",
            AppEffect::DeleteCharBackward,
        ),
        completion_trigger: register_simple(
            registry,
            "action:completion-trigger",
            "Insert mode's `<C-Space>` / `<C-x><C-o>`: trigger the completion popup.",
            AppEffect::CompletionTrigger,
        ),
        // SN.3c.1 (2026-06-14): the CommandSpec stays (the
        // `snippet-mode` chord binds it + the mode's global
        // `ActionHandlerRegistry` handler keys on it), but its
        // `apply` is now a dead `Effect::None`: the handler always
        // intercepts before the grammar Action gate, so this body
        // never runs. Kept (not deleted) so the `CommandId` resolves
        // for the chord binding + handler registration.
        snippet_expand: registry.register_action(
            "action:snippet-expand",
            "Insert mode's `<C-x><C-s>`: direct snippet expansion (mode-owned; \
             `snippet-mode`'s handler emits `Effect::ExpandSnippet`).",
            ActionSpec {
                apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        ),
        // L4b: command-name registration for lsp-diagnostics-mode's
        // `gl`. The mode's `ActionHandlerRegistry` closure intercepts
        // before this `apply`, so the body is a dead `Effect::None`
        // (same shape as `snippet_expand`). Kept so the `CommandId`
        // resolves for the chord binding + handler registration.
        lsp_diagnostic_popup: registry.register_action(
            "action:lsp-diagnostic-popup",
            "lsp-diagnostics-mode's `gl`: show the cursor line's diagnostics in a \
             cursor-anchored popup (mode-owned; the handler emits \
             `Effect::ShowDiagnosticsPopup`).",
            ActionSpec {
                apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        ),
        exit_visual: register_simple(
            registry,
            "action:exit-visual",
            "Visual mode's `<Esc>` / `v` / `V`: exit Visual to Normal.",
            AppEffect::ExitVisual,
        ),
        swap_visual_ends: register_simple(
            registry,
            "action:swap-visual-ends",
            "Visual mode's `o`: swap the cursor to the other end of the selection.",
            AppEffect::SwapVisualEnds,
        ),
        replace_undo_last: register_simple(
            registry,
            "action:replace-undo-last",
            "Replace mode's `<BS>`: undo the last overwritten char.",
            AppEffect::ReplaceUndoLast,
        ),
        enter_mode_insert: register_simple(
            registry,
            "action:enter-mode-insert",
            "Vim's `i`: enter Insert mode at the cursor.",
            AppEffect::EnterMode(ModalState::Insert),
        ),
        enter_mode_normal: register_simple(
            registry,
            "action:enter-mode-normal",
            "Vim's `<Esc>` (from Insert / Replace): return to Normal mode.",
            AppEffect::EnterMode(ModalState::Normal),
        ),
        enter_mode_replace: register_simple(
            registry,
            "action:enter-mode-replace",
            "Vim's `R`: enter Replace mode.",
            AppEffect::EnterMode(ModalState::Replace),
        ),
        enter_visual_charwise: register_simple(
            registry,
            "action:enter-visual-charwise",
            "Vim's `v`: enter charwise Visual at the current cursor.",
            AppEffect::EnterVisual(VisualKind::Charwise),
        ),
        enter_visual_linewise: register_simple(
            registry,
            "action:enter-visual-linewise",
            "Vim's `V`: enter linewise Visual at the current cursor.",
            AppEffect::EnterVisual(VisualKind::Linewise),
        ),
        enter_visual_blockwise: register_simple(
            registry,
            "action:enter-visual-blockwise",
            "Vim's `<C-v>` / `<C-q>`: enter blockwise Visual at the current cursor.",
            AppEffect::EnterVisual(VisualKind::Blockwise),
        ),
        enter_select_charwise: register_simple(
            registry,
            "action:enter-select-charwise",
            "Vim's `gh`: enter charwise Select at the current cursor (SN.3d).",
            AppEffect::EnterSelect(VisualKind::Charwise),
        ),
        enter_select_linewise: register_simple(
            registry,
            "action:enter-select-linewise",
            "Vim's `gH`: enter linewise Select at the current cursor (SN.3d).",
            AppEffect::EnterSelect(VisualKind::Linewise),
        ),
        enter_select_blockwise: register_simple(
            registry,
            "action:enter-select-blockwise",
            "Vim's `g<C-h>`: enter blockwise Select at the current cursor (SN.3d).",
            AppEffect::EnterSelect(VisualKind::Blockwise),
        ),
        enter_search_forward: register_simple(
            registry,
            "action:enter-search-forward",
            "Vim's `/`: enter the Search minibuffer searching forward.",
            AppEffect::EnterSearch(SearchDirection::Forward),
        ),
        enter_search_backward: register_simple(
            registry,
            "action:enter-search-backward",
            "Vim's `?`: enter the Search minibuffer searching backward.",
            AppEffect::EnterSearch(SearchDirection::Backward),
        ),
        search_word_under_cursor_forward: register_simple(
            registry,
            "action:search-word-under-cursor-forward",
            "Vim's `*`: search forward for the word under the cursor.",
            AppEffect::SearchWordUnderCursor(SearchDirection::Forward),
        ),
        search_word_under_cursor_backward: register_simple(
            registry,
            "action:search-word-under-cursor-backward",
            "Vim's `#`: search backward for the word under the cursor.",
            AppEffect::SearchWordUnderCursor(SearchDirection::Backward),
        ),
        jump_viewport_top: register_simple(
            registry,
            "action:jump-viewport-top",
            "Vim's `H`: jump cursor to the top visible line.",
            AppEffect::JumpViewport(ViewportPos::Top),
        ),
        jump_viewport_middle: register_simple(
            registry,
            "action:jump-viewport-middle",
            "Vim's `M`: jump cursor to the middle visible line.",
            AppEffect::JumpViewport(ViewportPos::Middle),
        ),
        jump_viewport_bottom: register_simple(
            registry,
            "action:jump-viewport-bottom",
            "Vim's `L`: jump cursor to the bottom visible line.",
            AppEffect::JumpViewport(ViewportPos::Bottom),
        ),
        scroll_cursor_to_top: register_simple(
            registry,
            "action:scroll-cursor-to-top",
            "Vim's `zt`: scroll viewport so the cursor's line sits at the top.",
            AppEffect::ScrollCursorTo(ScrollPos::Top),
        ),
        scroll_cursor_to_center: register_simple(
            registry,
            "action:scroll-cursor-to-center",
            "Vim's `zz`: scroll viewport so the cursor's line sits at the centre.",
            AppEffect::ScrollCursorTo(ScrollPos::Center),
        ),
        scroll_cursor_to_bottom: register_simple(
            registry,
            "action:scroll-cursor-to-bottom",
            "Vim's `zb`: scroll viewport so the cursor's line sits at the bottom.",
            AppEffect::ScrollCursorTo(ScrollPos::Bottom),
        ),
        h_scroll_right: register_simple(
            registry,
            "action:h-scroll-right",
            "Vim's `zl`: scroll the view right by [count] columns (wrap off).",
            AppEffect::HorizontalScroll(HScroll::Columns { right: true }),
        ),
        h_scroll_left: register_simple(
            registry,
            "action:h-scroll-left",
            "Vim's `zh`: scroll the view left by [count] columns (wrap off).",
            AppEffect::HorizontalScroll(HScroll::Columns { right: false }),
        ),
        h_scroll_half_right: register_simple(
            registry,
            "action:h-scroll-half-right",
            "Vim's `zL`: scroll the view right by half the body width.",
            AppEffect::HorizontalScroll(HScroll::HalfScreen { right: true }),
        ),
        h_scroll_half_left: register_simple(
            registry,
            "action:h-scroll-half-left",
            "Vim's `zH`: scroll the view left by half the body width.",
            AppEffect::HorizontalScroll(HScroll::HalfScreen { right: false }),
        ),
        h_scroll_cursor_left_edge: register_simple(
            registry,
            "action:h-scroll-cursor-left-edge",
            "Vim's `zs`: scroll so the cursor's column sits at the left edge.",
            AppEffect::HorizontalScroll(HScroll::CursorToEdge { end: false }),
        ),
        h_scroll_cursor_right_edge: register_simple(
            registry,
            "action:h-scroll-cursor-right-edge",
            "Vim's `ze`: scroll so the cursor's column sits at the right edge.",
            AppEffect::HorizontalScroll(HScroll::CursorToEdge { end: true }),
        ),
        join_lines_with_space: register_simple(
            registry,
            "action:join-lines-with-space",
            "Vim's `J`: join the current line with the next using a single space.",
            AppEffect::JoinLines { with_space: true },
        ),
        join_lines_bare: register_simple(
            registry,
            "action:join-lines-bare",
            "Vim's `gJ`: join the current line with the next without inserting a space.",
            AppEffect::JoinLines { with_space: false },
        ),
        find_repeat_forward: register_simple(
            registry,
            "action:find-repeat-forward",
            "Vim's `;`: repeat the most recent f/F/t/T find in the same direction.",
            AppEffect::FindRepeat { reverse: false },
        ),
        find_repeat_reverse: register_simple(
            registry,
            "action:find-repeat-reverse",
            "Vim's `,`: repeat the most recent f/F/t/T find in the reverse direction.",
            AppEffect::FindRepeat { reverse: true },
        ),
        insert_newline: register_simple(
            registry,
            "action:insert-newline",
            "Insert / Replace mode's `<CR>`: insert a literal newline at the cursor.",
            AppEffect::InsertNewline,
        ),
        insert_tab: register_simple(
            registry,
            "action:insert-tab",
            "Insert mode's `<Tab>`: insert a literal tab at the cursor.",
            AppEffect::InsertTab,
        ),
        overwrite_char: register_action(
            registry,
            "action:overwrite-char",
            "Replace mode's bare-printable wildcard: overwrite the byte at the cursor with the captured char.",
            captured_char_action(|c| Some(AppEffect::OverwriteChar(c))),
        ),
        set_mark: register_action(
            registry,
            "action:set-mark",
            "Vim's `m<X>`: set mark `<X>` at the cursor (alphanumeric only).",
            captured_char_action(|c| {
                if c.is_ascii_alphanumeric() {
                    Some(AppEffect::SetMark(c))
                } else {
                    None
                }
            }),
        ),
        jump_to_mark_line: register_action(
            registry,
            "action:jump-to-mark-line",
            "Vim's `'<X>`: jump cursor to the line of mark `<X>` (alphanumeric only).",
            captured_char_action(|c| {
                if c.is_ascii_alphanumeric() {
                    Some(AppEffect::JumpToMarkLine(c))
                } else {
                    None
                }
            }),
        ),
        jump_to_mark_exact: register_action(
            registry,
            "action:jump-to-mark-exact",
            "Vim's `` `<X> ``: jump cursor to the exact position of mark `<X>`.",
            captured_char_action(|c| {
                if c.is_ascii_alphanumeric() {
                    Some(AppEffect::JumpToMarkExact(c))
                } else {
                    None
                }
            }),
        ),
        select_register: register_action(
            registry,
            "action:select-register",
            "Vim's `\"<X>`: select named register `<X>` for the next yank / paste / delete.",
            captured_char_action(|c| Register::from_input_char(c).map(AppEffect::SelectRegister)),
        ),
        start_macro_record: register_action(
            registry,
            "action:start-macro-record",
            "Vim's `q<X>`: start recording a macro into register `<X>` (alphanumeric only).",
            captured_char_action(|c| {
                if c.is_ascii_alphanumeric() {
                    Some(AppEffect::StartMacroRecord(c))
                } else {
                    None
                }
            }),
        ),
        play_macro: register_action(
            registry,
            "action:play-macro",
            "Vim's `@<X>`: play the macro stored in register `<X>`. `@@` replays the most recent macro.",
            captured_char_action(|c| {
                if c == '@' {
                    Some(AppEffect::PlayLastMacro)
                } else if c.is_ascii_alphanumeric() {
                    Some(AppEffect::PlayMacro(c))
                } else {
                    None
                }
            }),
        ),
        absorb_operator_delete: register_operator_prefix(
            registry,
            "action:absorb-operator-delete",
            "Vim's `d`: arm operator-pending for delete.",
            builtins.delete,
        ),
        absorb_operator_change: register_operator_prefix(
            registry,
            "action:absorb-operator-change",
            "Vim's `c`: arm operator-pending for change.",
            builtins.change,
        ),
        absorb_operator_yank: register_operator_prefix(
            registry,
            "action:absorb-operator-yank",
            "Vim's `y`: arm operator-pending for yank.",
            builtins.yank,
        ),
        absorb_operator_indent_right: register_operator_prefix(
            registry,
            "action:absorb-operator-indent-right",
            "Vim's `>`: arm operator-pending for indent-right.",
            builtins.indent_right,
        ),
        absorb_operator_indent_left: register_operator_prefix(
            registry,
            "action:absorb-operator-indent-left",
            "Vim's `<`: arm operator-pending for indent-left.",
            builtins.indent_left,
        ),
        absorb_operator_upper: register_operator_prefix(
            registry,
            "action:absorb-operator-upper",
            "Vim's `gU`: arm operator-pending for uppercase.",
            builtins.upper,
        ),
        absorb_operator_lower: register_operator_prefix(
            registry,
            "action:absorb-operator-lower",
            "Vim's `gu`: arm operator-pending for lowercase.",
            builtins.lower,
        ),
        absorb_operator_toggle_case: register_operator_prefix(
            registry,
            "action:absorb-operator-toggle-case",
            "Vim's `g~`: arm operator-pending for toggle-case.",
            builtins.toggle_case,
        ),
        split_pane_horizontal: register_simple(
            registry,
            "action:split-pane-horizontal",
            "Vim's `<C-w>s`: split the active pane horizontally.",
            AppEffect::SplitPaneHorizontal,
        ),
        split_pane_vertical: register_simple(
            registry,
            "action:split-pane-vertical",
            "Vim's `<C-w>v`: split the active pane vertically.",
            AppEffect::SplitPaneVertical,
        ),
        close_pane: register_simple(
            registry,
            "action:close-pane",
            "Vim's `<C-w>c`: close the active pane.",
            AppEffect::ClosePane,
        ),
        only_pane: register_simple(
            registry,
            "action:only-pane",
            "Vim's `<C-w>o` / `:only`: close every pane except the active one.",
            AppEffect::OnlyPane,
        ),
        navigate_pane_left: register_simple(
            registry,
            "action:navigate-pane-left",
            "Vim's `<C-w>h`: move focus to the pane on the left.",
            AppEffect::NavigatePane(PaneDirection::Left),
        ),
        navigate_pane_down: register_simple(
            registry,
            "action:navigate-pane-down",
            "Vim's `<C-w>j`: move focus to the pane below.",
            AppEffect::NavigatePane(PaneDirection::Down),
        ),
        navigate_pane_up: register_simple(
            registry,
            "action:navigate-pane-up",
            "Vim's `<C-w>k`: move focus to the pane above.",
            AppEffect::NavigatePane(PaneDirection::Up),
        ),
        navigate_pane_right: register_simple(
            registry,
            "action:navigate-pane-right",
            "Vim's `<C-w>l`: move focus to the pane on the right.",
            AppEffect::NavigatePane(PaneDirection::Right),
        ),
        next_pane: register_simple(
            registry,
            "action:next-pane",
            "Vim's `<C-w>w`: cycle focus to the next pane.",
            AppEffect::NextPane,
        ),
        prev_pane: register_simple(
            registry,
            "action:prev-pane",
            "Vim's `<C-w>W`: cycle focus to the previous pane.",
            AppEffect::PrevPane,
        ),
        // Issue #28 (2026-05-22): split-ratio adjustment.
        equalize_panes: register_simple(
            registry,
            "action:equalize-panes",
            "Vim's `<C-w>=`: reset every split's ratio to 0.5.",
            AppEffect::EqualizePanes,
        ),
        grow_pane_height: register_simple(
            registry,
            "action:grow-pane-height",
            "Vim's `<C-w>+`: grow the active pane vertically.",
            AppEffect::GrowPaneHeight,
        ),
        shrink_pane_height: register_simple(
            registry,
            "action:shrink-pane-height",
            "Vim's `<C-w>-`: shrink the active pane vertically.",
            AppEffect::ShrinkPaneHeight,
        ),
        grow_pane_width: register_simple(
            registry,
            "action:grow-pane-width",
            "Vim's `<C-w>>`: grow the active pane horizontally.",
            AppEffect::GrowPaneWidth,
        ),
        shrink_pane_width: register_simple(
            registry,
            "action:shrink-pane-width",
            "Vim's `<C-w><`: shrink the active pane horizontally.",
            AppEffect::ShrinkPaneWidth,
        ),
        // Issue #29 (2026-05-22): tab management.
        // Issue #32 (2026-05-22): picker open-target overrides.
        picker_accept_in_split: register_simple(
            registry,
            "action:picker-accept-in-split",
            "Picker `<C-s>`: accept candidate in a horizontal split.",
            AppEffect::PickerAcceptInSplit,
        ),
        picker_accept_in_vsplit: register_simple(
            registry,
            "action:picker-accept-in-vsplit",
            "Picker `<C-v>`: accept candidate in a vertical split.",
            AppEffect::PickerAcceptInVSplit,
        ),
        picker_accept_in_tab: register_simple(
            registry,
            "action:picker-accept-in-tab",
            "Picker `<C-t>`: accept candidate in a new tab.",
            AppEffect::PickerAcceptInTab,
        ),
        next_tab: register_simple(
            registry,
            "action:next-tab",
            "Vim's `gt`: switch to the next tab.",
            AppEffect::NextTab,
        ),
        prev_tab: register_simple(
            registry,
            "action:prev-tab",
            "Vim's `gT`: switch to the previous tab.",
            AppEffect::PrevTab,
        ),
        new_tab: register_simple(
            registry,
            "action:new-tab",
            "`:tabnew`: open a new tab.",
            AppEffect::NewTab,
        ),
        close_tab: register_simple(
            registry,
            "action:close-tab",
            "`:tabclose`: close the active tab.",
            AppEffect::CloseTab,
        ),
        only_tab: register_simple(
            registry,
            "action:only-tab",
            "`:tabonly`: close every tab except the active one.",
            AppEffect::OnlyTab,
        ),
        // move-tab carries no fixed target; the dispatch path
        // surfaces it via the count-aware `Action::MoveTab(n)`
        // payload. Registered with MoveTab(0) (= move to last)
        // as a sensible no-arg default.
        move_tab: register_simple(
            registry,
            "action:move-tab",
            "`:tabmove [N]`: move the active tab to position N (default last).",
            AppEffect::MoveTab(0),
        ),
        move_pane_to_new_tab: register_simple(
            registry,
            "action:move-pane-to-new-tab",
            "`<C-w>T`: move the active pane to a fresh tab.",
            AppEffect::MovePaneToNewTab,
        ),
        completion_next: register_simple(
            registry,
            "action:completion-next",
            "Completion-popup `<C-n>` / `<Down>`: focus the next entry.",
            AppEffect::CompletionNext,
        ),
        completion_prev: register_simple(
            registry,
            "action:completion-prev",
            "Completion-popup `<C-p>` / `<Up>`: focus the previous entry.",
            AppEffect::CompletionPrev,
        ),
        completion_accept: register_simple(
            registry,
            "action:completion-accept",
            "Completion-popup `<C-y>` / `<Tab>` / `<CR>`: accept the focused candidate.",
            AppEffect::CompletionAccept,
        ),
        completion_cancel: register_simple(
            registry,
            "action:completion-cancel",
            "Completion-popup `<C-e>`: cancel the popup, stay in Insert.",
            AppEffect::CompletionCancel,
        ),
        completion_cancel_and_exit_insert: register_simple(
            registry,
            "action:completion-cancel-and-exit-insert",
            "Completion-popup `<Esc>`: cancel the popup and exit Insert.",
            AppEffect::CompletionCancelAndExitInsert,
        ),
        completion_toggle_docs: register_simple(
            registry,
            "action:completion-toggle-docs",
            "Completion-popup `<C-d>`: toggle the doc popup.",
            AppEffect::CompletionToggleDocs,
        ),
        completion_docs_scroll_down: register_simple(
            registry,
            "action:completion-docs-scroll-down",
            "Completion-popup `<C-f>`: scroll the doc popup down.",
            AppEffect::CompletionDocsScrollDown,
        ),
        completion_docs_scroll_up: register_simple(
            registry,
            "action:completion-docs-scroll-up",
            "Completion-popup `<C-b>`: scroll the doc popup up.",
            AppEffect::CompletionDocsScrollUp,
        ),
        completion_accept_then_insert: register_action(
            registry,
            "action:completion-accept-then-insert",
            "Completion-popup bare-printable wildcard: accept then insert the captured char (control chars filtered).",
            captured_char_action(|c| {
                // Mirror the legacy popup's `if !c.is_control()`
                // filter so synthetic control chars don't leak
                // into the inserted text via the trie's
                // CharLiteral wildcard.
                if c.is_control() {
                    None
                } else {
                    Some(AppEffect::CompletionAcceptThenInsert(c))
                }
            }),
        ),
        completion_filter_to_source: register_action(
            registry,
            "action:completion-filter-to-source",
            "Completion-popup filter chord: restrict candidates to a single source (Args::String = source-id).",
            captured_string_action(|id| {
                if id.is_empty() {
                    None
                } else {
                    Some(AppEffect::CompletionFilterToSource(id))
                }
            }),
        ),
        completion_filter_clear: register_simple(
            registry,
            "action:completion-filter-clear",
            "Completion-popup `<C-Space>`: clear the active source filter.",
            AppEffect::CompletionFilterClear,
        ),
        // CR.6: `action:diff-get`/`-put` registered by lattice_diff::install().
        snippet_next_placeholder: register_simple(
            registry,
            "action:snippet-next-placeholder",
            "Active-snippet `<Tab>`: jump to the next placeholder.",
            AppEffect::SnippetNextPlaceholder,
        ),
        snippet_prev_placeholder: register_simple(
            registry,
            "action:snippet-prev-placeholder",
            "Active-snippet `<S-Tab>`: jump to the previous placeholder.",
            AppEffect::SnippetPrevPlaceholder,
        ),
        // SN.3c.2 (2026-06-14): the CommandSpec stays (the
        // `active-snippet-mode` chord binds it + the mode's
        // per-buffer `ActionHandlerRegistry` handler keys on it), but
        // its `apply` is now a dead `Effect::None`: the handler always
        // intercepts before the grammar Action gate, so this body
        // never runs. Same shape as `snippet_expand` (SN.3c.1).
        snippet_leave: registry.register_action(
            "action:snippet-leave",
            "Active-snippet `<Esc>`: exit the snippet (mode-owned; \
             `active-snippet-mode`'s handler clears the session + enters Normal).",
            ActionSpec {
                apply: Box::new(|_ctx| Ok(lattice_grammar::Effect::None)),
                args_schema: vec![],
            },
        ),
        search_jump_to_source: register_simple(
            registry,
            "action:search-jump-to-source",
            "project-search-multibuffer-mode `<CR>`: jump to source file/row of the excerpt under cursor.",
            AppEffect::SearchJumpToSource,
        ),
        search_refresh: register_simple(
            registry,
            "action:search-refresh",
            "project-search-multibuffer-mode `gr`: re-run the scan with the view's current query.",
            AppEffect::SearchRefresh,
        ),
    }
}

/// Helper for the 8 operator-prefix actions (slice 8.i.4.c).
/// Captures the `OperatorId` in the closure so each registration
/// returns a constant `Effect::AppAction(AppEffect::AbsorbOperatorPrefix(op))`
/// for its specific operator.
fn register_operator_prefix(
    registry: &mut CommandRegistry,
    name: &str,
    doc: &str,
    op: OperatorId,
) -> CommandId {
    registry.register_action(
        name,
        doc,
        ActionSpec {
            apply: Box::new(move |_| Ok(Effect::AppAction(AppEffect::AbsorbOperatorPrefix(op)))),
            args_schema: vec![],
        },
    )
}

/// Helper for captured-char wildcard actions (`m<X>`, `'<X>`,
/// `\"<X>`, `q<X>`, `@<X>`, Replace mode's wildcard).
///
/// The dispatcher folds the captured char into the bound
/// `CommandInvocation`'s `args` as `Args::Char(c)`. This helper
/// reads it back, hands it to `decide`, and emits either the
/// chosen `AppEffect` or a no-op when the char doesn't validate.
/// `Effect::None` is the no-op signal: `App::apply` clears any
/// in-flight pending state on every Invoke (the pending field
/// only survives `Action::SetPending(_)`), so a no-op naturally
/// drops the pending half-chord without an explicit
/// `SetPending(None)` round-trip.
fn captured_char_action(
    decide: impl Fn(char) -> Option<AppEffect> + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec {
        apply: Box::new(move |ctx| {
            let c = match ctx.args {
                Args::Char(c) => c,
                _ => return Ok(Effect::None),
            };
            Ok(match decide(c) {
                Some(eff) => Effect::AppAction(eff),
                None => Effect::None,
            })
        }),
        args_schema: vec![],
    }
}

/// Helper for captured-string actions (CSM.K2 filter chords).
/// Reads `Args::String(s)` and hands the owned `String` to
/// `decide`. Behaves identically to `captured_char_action`
/// otherwise -- mismatched args / `None` decisions are
/// `Effect::None` so any in-flight pending state is dropped.
fn captured_string_action(
    decide: impl Fn(String) -> Option<AppEffect> + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec {
        apply: Box::new(move |ctx| {
            let s = match &ctx.args {
                Args::String(s) => s.clone(),
                _ => return Ok(Effect::None),
            };
            Ok(match decide(s) {
                Some(eff) => Effect::AppAction(eff),
                None => Effect::None,
            })
        }),
        args_schema: vec![],
    }
}

/// Tiny adapter so the pattern-rich registrations above can keep
/// the same call-site shape as `register_simple` but pass an
/// already-built `ActionSpec`. Mirrors `register_simple`'s wrap
/// of `CommandRegistry::register_action`.
fn register_action(
    registry: &mut CommandRegistry,
    name: &str,
    doc: &str,
    spec: ActionSpec,
) -> CommandId {
    registry.register_action(name, doc, spec)
}

/// Helper for the common case: an action whose `apply` is the
/// constant `Effect::AppAction(AppEffect::Foo)`. Most slice 8.i
/// promotions look like this. Variants that need to inspect
/// args / count / register at dispatch time call
/// `register_action` directly.
fn register_simple(
    registry: &mut CommandRegistry,
    name: &str,
    doc: &str,
    effect: AppEffect,
) -> CommandId {
    registry.register_action(
        name,
        doc,
        ActionSpec {
            apply: Box::new(move |_ctx| Ok(lattice_grammar::Effect::AppAction(effect.clone()))),
            args_schema: vec![],
        },
    )
}

// CR.6 (2026-06-24): `register_mode_owned` removed — the diff conflict
// action shells it served are now registered in `lattice_diff::install()`
// (the "modes register commands" pattern).

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_grammar::CancellationToken;
    use lattice_grammar::CommandInvocation;
    use lattice_grammar::Effect;
    use lattice_grammar::dispatcher::execute;

    #[test]
    fn populate_registers_every_field_into_registry() {
        let mut registry = CommandRegistry::new();
        let builtins = lattice_grammar::builtins::populate(&mut registry);
        let ids = populate(&mut registry, &builtins);
        // Every field should round-trip back to a registered
        // `CommandKind::Action` entry that names the dashed form.
        for (id, expected_name) in [
            (ids.match_bracket, "action:match-bracket"),
            (ids.toggle_case_at_cursor, "action:toggle-case-at-cursor"),
            (ids.open_line_below, "action:open-line-below"),
            (ids.open_line_above, "action:open-line-above"),
            (ids.lsp_hover_request, "action:lsp-hover"),
            (ids.search_next, "action:search-next"),
            (ids.search_previous, "action:search-previous"),
            (ids.jump_history_back, "action:jump-history-back"),
            (ids.jump_history_forward, "action:jump-history-forward"),
            (ids.walk_mark_history_back, "action:walk-mark-history-back"),
            (
                ids.walk_mark_history_forward,
                "action:walk-mark-history-forward",
            ),
            (ids.tag_stack_pop, "action:tag-stack-pop"),
            (ids.open_fold_at_cursor, "action:open-fold-at-cursor"),
            (ids.close_fold_at_cursor, "action:close-fold-at-cursor"),
            (ids.toggle_fold_at_cursor, "action:toggle-fold-at-cursor"),
            (ids.open_all_folds, "action:open-all-folds"),
            (ids.close_all_folds, "action:close-all-folds"),
            (ids.cycle_fold_at_cursor, "action:fold-cycle"),
            (ids.cycle_folds_global, "action:fold-cycle-global"),
            (ids.goto_parent_fold, "action:fold-goto-parent"),
            (ids.delete_fold_at_cursor, "action:delete-fold-at-cursor"),
            (ids.goto_next_fold, "action:goto-next-fold"),
            (ids.goto_prev_fold, "action:goto-prev-fold"),
            (ids.toggle_fold_enable, "action:toggle-fold-enable"),
            (ids.undo, "action:undo"),
            (ids.redo, "action:redo"),
            (ids.repeat_last_change, "action:repeat-last-change"),
            (ids.page_down, "action:page-down"),
            (ids.page_up, "action:page-up"),
            (ids.scroll_line_up, "action:scroll-line-up"),
            (ids.scroll_line_down, "action:scroll-line-down"),
            (ids.redraw_screen, "action:redraw-screen"),
            (ids.open_command_picker, "action:open-command-picker"),
            (ids.enter_command_line, "action:enter-command-line"),
            (ids.oil_navigate_up, "action:oil-navigate-up"),
            (ids.reselect_last_visual, "action:reselect-last-visual"),
            (ids.paste_after, "action:paste-after"),
            (ids.paste_before, "action:paste-before"),
            (ids.lsp_definition_request, "action:lsp-definition"),
            (ids.lsp_declaration_request, "action:lsp-declaration"),
            (
                ids.lsp_type_definition_request,
                "action:lsp-type-definition",
            ),
            (ids.lsp_implementation_request, "action:lsp-implementation"),
            (ids.lsp_references_request, "action:lsp-references"),
            (ids.lsp_follow_link_at_cursor, "action:lsp-follow-link"),
            (ids.enter_append, "action:enter-append"),
            (
                ids.enter_insert_first_non_blank,
                "action:enter-insert-first-non-blank",
            ),
            (
                ids.enter_append_end_of_line,
                "action:enter-append-end-of-line",
            ),
            (ids.display_line_down, "action:display-line-down"),
            (ids.display_line_up, "action:display-line-up"),
            (ids.display_line_start, "action:display-line-start"),
            (ids.display_line_end, "action:display-line-end"),
            (
                ids.create_fold_from_visual,
                "action:create-fold-from-visual",
            ),
            (ids.delete_char_backward, "action:delete-char-backward"),
            (ids.completion_trigger, "action:completion-trigger"),
            (ids.snippet_expand, "action:snippet-expand"),
            (ids.lsp_diagnostic_popup, "action:lsp-diagnostic-popup"),
            (ids.exit_visual, "action:exit-visual"),
            (ids.swap_visual_ends, "action:swap-visual-ends"),
            (ids.replace_undo_last, "action:replace-undo-last"),
            (ids.enter_mode_insert, "action:enter-mode-insert"),
            (ids.enter_mode_normal, "action:enter-mode-normal"),
            (ids.enter_mode_replace, "action:enter-mode-replace"),
            (ids.enter_visual_charwise, "action:enter-visual-charwise"),
            (ids.enter_visual_linewise, "action:enter-visual-linewise"),
            (ids.enter_visual_blockwise, "action:enter-visual-blockwise"),
            (ids.enter_search_forward, "action:enter-search-forward"),
            (ids.enter_search_backward, "action:enter-search-backward"),
            (
                ids.search_word_under_cursor_forward,
                "action:search-word-under-cursor-forward",
            ),
            (
                ids.search_word_under_cursor_backward,
                "action:search-word-under-cursor-backward",
            ),
            (ids.jump_viewport_top, "action:jump-viewport-top"),
            (ids.jump_viewport_middle, "action:jump-viewport-middle"),
            (ids.jump_viewport_bottom, "action:jump-viewport-bottom"),
            (ids.scroll_cursor_to_top, "action:scroll-cursor-to-top"),
            (
                ids.scroll_cursor_to_center,
                "action:scroll-cursor-to-center",
            ),
            (
                ids.scroll_cursor_to_bottom,
                "action:scroll-cursor-to-bottom",
            ),
            (ids.join_lines_with_space, "action:join-lines-with-space"),
            (ids.join_lines_bare, "action:join-lines-bare"),
            (ids.find_repeat_forward, "action:find-repeat-forward"),
            (ids.find_repeat_reverse, "action:find-repeat-reverse"),
            (ids.insert_newline, "action:insert-newline"),
            (ids.insert_tab, "action:insert-tab"),
            (ids.overwrite_char, "action:overwrite-char"),
            (ids.set_mark, "action:set-mark"),
            (ids.jump_to_mark_line, "action:jump-to-mark-line"),
            (ids.jump_to_mark_exact, "action:jump-to-mark-exact"),
            (ids.select_register, "action:select-register"),
            (ids.start_macro_record, "action:start-macro-record"),
            (ids.play_macro, "action:play-macro"),
            (ids.absorb_operator_delete, "action:absorb-operator-delete"),
            (ids.absorb_operator_change, "action:absorb-operator-change"),
            (ids.absorb_operator_yank, "action:absorb-operator-yank"),
            (
                ids.absorb_operator_indent_right,
                "action:absorb-operator-indent-right",
            ),
            (
                ids.absorb_operator_indent_left,
                "action:absorb-operator-indent-left",
            ),
            (ids.absorb_operator_upper, "action:absorb-operator-upper"),
            (ids.absorb_operator_lower, "action:absorb-operator-lower"),
            (
                ids.absorb_operator_toggle_case,
                "action:absorb-operator-toggle-case",
            ),
            (ids.split_pane_horizontal, "action:split-pane-horizontal"),
            (ids.split_pane_vertical, "action:split-pane-vertical"),
            (ids.close_pane, "action:close-pane"),
            (ids.only_pane, "action:only-pane"),
            (ids.navigate_pane_left, "action:navigate-pane-left"),
            (ids.navigate_pane_down, "action:navigate-pane-down"),
            (ids.navigate_pane_up, "action:navigate-pane-up"),
            (ids.navigate_pane_right, "action:navigate-pane-right"),
            (ids.next_pane, "action:next-pane"),
            (ids.prev_pane, "action:prev-pane"),
            // Issue #28 (2026-05-22): split-ratio adjustment.
            (ids.equalize_panes, "action:equalize-panes"),
            (ids.grow_pane_height, "action:grow-pane-height"),
            (ids.shrink_pane_height, "action:shrink-pane-height"),
            (ids.grow_pane_width, "action:grow-pane-width"),
            (ids.shrink_pane_width, "action:shrink-pane-width"),
            // Issue #29 (2026-05-22): tab management.
            // Issue #32 (2026-05-22): picker open-target overrides.
            (ids.picker_accept_in_split, "action:picker-accept-in-split"),
            (
                ids.picker_accept_in_vsplit,
                "action:picker-accept-in-vsplit",
            ),
            (ids.picker_accept_in_tab, "action:picker-accept-in-tab"),
            (ids.next_tab, "action:next-tab"),
            (ids.prev_tab, "action:prev-tab"),
            (ids.new_tab, "action:new-tab"),
            (ids.close_tab, "action:close-tab"),
            (ids.only_tab, "action:only-tab"),
            (ids.move_tab, "action:move-tab"),
            (ids.move_pane_to_new_tab, "action:move-pane-to-new-tab"),
            (ids.completion_next, "action:completion-next"),
            (ids.completion_prev, "action:completion-prev"),
            (ids.completion_accept, "action:completion-accept"),
            (ids.completion_cancel, "action:completion-cancel"),
            (
                ids.completion_cancel_and_exit_insert,
                "action:completion-cancel-and-exit-insert",
            ),
            (ids.completion_toggle_docs, "action:completion-toggle-docs"),
            (
                ids.completion_docs_scroll_down,
                "action:completion-docs-scroll-down",
            ),
            (
                ids.completion_docs_scroll_up,
                "action:completion-docs-scroll-up",
            ),
            (
                ids.completion_accept_then_insert,
                "action:completion-accept-then-insert",
            ),
            (
                ids.completion_filter_to_source,
                "action:completion-filter-to-source",
            ),
            (
                ids.completion_filter_clear,
                "action:completion-filter-clear",
            ),
            (
                ids.snippet_next_placeholder,
                "action:snippet-next-placeholder",
            ),
            (
                ids.snippet_prev_placeholder,
                "action:snippet-prev-placeholder",
            ),
            (ids.snippet_leave, "action:snippet-leave"),
        ] {
            let spec = registry
                .lookup(id)
                .unwrap_or_else(|| panic!("missing registry entry for `{expected_name}`"));
            assert_eq!(spec.name, expected_name);
        }
    }

    #[test]
    fn dispatch_returns_app_action_effect() {
        let mut registry = CommandRegistry::new();
        let builtins = lattice_grammar::builtins::populate(&mut registry);
        let ids = populate(&mut registry, &builtins);
        let mut doc = lattice_core::Document::empty();
        let inv = CommandInvocation::of(ids.match_bracket);
        let eff = execute(
            &registry,
            &mut doc,
            lattice_core::BufferId(0),
            lattice_protocol::position::Position::ZERO,
            inv,
            &CancellationToken::never(),
        )
        .unwrap();
        match eff {
            Effect::AppAction(AppEffect::MatchBracket) => {}
            other => panic!("expected MatchBracket, got {other:?}"),
        }
    }

    /// Slice 8.i.3: captured-char `ActionSpec`s validate the
    /// captured char and return `Effect::None` on rejection. The
    /// dispatcher always passes through; the no-op effect is the
    /// signal that the pending half-chord should drop without an
    /// explicit `SetPending(None)` round-trip.
    #[test]
    fn captured_char_specs_validate() {
        let mut registry = CommandRegistry::new();
        let builtins = lattice_grammar::builtins::populate(&mut registry);
        let ids = populate(&mut registry, &builtins);
        let mut doc = lattice_core::Document::empty();
        let cancel = CancellationToken::never();
        let pos = lattice_protocol::position::Position::ZERO;

        let dispatch_with_char = |id: lattice_protocol::ids::CommandId,
                                  c: char,
                                  registry: &CommandRegistry,
                                  doc: &mut lattice_core::Document|
         -> Effect {
            execute(
                registry,
                doc,
                lattice_core::BufferId(0),
                pos,
                CommandInvocation::of(id).with_args(Args::Char(c)),
                &cancel,
            )
            .unwrap()
        };

        // Set-mark: alphanumeric pass, punctuation reject.
        match dispatch_with_char(ids.set_mark, 'a', &registry, &mut doc) {
            Effect::AppAction(AppEffect::SetMark('a')) => {}
            other => panic!("expected SetMark('a'), got {other:?}"),
        }
        match dispatch_with_char(ids.set_mark, '!', &registry, &mut doc) {
            Effect::None => {}
            other => panic!("expected None for invalid mark name, got {other:?}"),
        }

        // Select-register: routes through `Register::from_input_char`.
        match dispatch_with_char(ids.select_register, '+', &registry, &mut doc) {
            Effect::AppAction(AppEffect::SelectRegister(Register::System)) => {}
            other => panic!("expected SelectRegister(System), got {other:?}"),
        }
        match dispatch_with_char(ids.select_register, 'a', &registry, &mut doc) {
            Effect::AppAction(AppEffect::SelectRegister(Register::Named('a'))) => {}
            other => panic!("expected SelectRegister(Named('a')), got {other:?}"),
        }
        match dispatch_with_char(ids.select_register, '@', &registry, &mut doc) {
            Effect::None => {}
            other => panic!("expected None for invalid register char, got {other:?}"),
        }

        // Play-macro: `@` -> PlayLastMacro; alphanumeric ->
        // PlayMacro(c); other -> None.
        match dispatch_with_char(ids.play_macro, '@', &registry, &mut doc) {
            Effect::AppAction(AppEffect::PlayLastMacro) => {}
            other => panic!("expected PlayLastMacro, got {other:?}"),
        }
        match dispatch_with_char(ids.play_macro, 'q', &registry, &mut doc) {
            Effect::AppAction(AppEffect::PlayMacro('q')) => {}
            other => panic!("expected PlayMacro('q'), got {other:?}"),
        }
        match dispatch_with_char(ids.play_macro, '!', &registry, &mut doc) {
            Effect::None => {}
            other => panic!("expected None for invalid macro key, got {other:?}"),
        }

        // Overwrite-char: any char passes.
        match dispatch_with_char(ids.overwrite_char, '$', &registry, &mut doc) {
            Effect::AppAction(AppEffect::OverwriteChar('$')) => {}
            other => panic!("expected OverwriteChar('$'), got {other:?}"),
        }
    }
}
