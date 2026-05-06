//! App-side action registrations -- the `CommandKind::Action`
//! peers of the grammar's built-in motions / operators / text-
//! objects (`lattice_grammar::builtins`) and built-in ex-commands
//! (`lattice_grammar::ex_commands`).
//!
//! See `docs/8i-approach.md` for the slice 8.i plan. Each action
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
use lattice_grammar::registry::ActionSpec;
use lattice_protocol::ids::CommandId;

/// Strongly-typed handles to every App-side action registered
/// in the global [`CommandRegistry`]. Mirrors the shape of
/// `lattice_grammar::builtins::Builtins`: each field is the
/// `CommandId` produced by [`CommandRegistry::register_action`]
/// at startup. The App stores this struct; per-mode keymap
/// modules consume it to build typed `CommandInvocation`s for
/// chord bindings.
#[derive(Debug, Clone, Copy)]
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
    pub enter_append: CommandId,
    pub create_fold_from_visual: CommandId,
    pub delete_char_backward: CommandId,
    pub completion_trigger: CommandId,
    pub snippet_expand: CommandId,
}

/// Register every App-side action into `registry` and return
/// the resulting [`ActionIds`]. Called once at App startup,
/// after `lattice_grammar::builtins::populate` and
/// `lattice_grammar::ex_commands::populate`.
pub fn populate(registry: &mut CommandRegistry) -> ActionIds {
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
        lsp_hover_request: register_simple(
            registry,
            "action:lsp-hover",
            "`K`: send `textDocument/hover` to every attached LSP server.",
            AppEffect::LspHoverRequest,
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
        lsp_definition_request: register_simple(
            registry,
            "action:lsp-definition",
            "`gd`: send `textDocument/definition` to attached LSP servers.",
            AppEffect::LspDefinitionRequest,
        ),
        lsp_declaration_request: register_simple(
            registry,
            "action:lsp-declaration",
            "`gD`: send `textDocument/declaration` to attached LSP servers.",
            AppEffect::LspDeclarationRequest,
        ),
        lsp_type_definition_request: register_simple(
            registry,
            "action:lsp-type-definition",
            "`gy`: send `textDocument/typeDefinition` to attached LSP servers.",
            AppEffect::LspTypeDefinitionRequest,
        ),
        lsp_implementation_request: register_simple(
            registry,
            "action:lsp-implementation",
            "`gI`: send `textDocument/implementation` to attached LSP servers.",
            AppEffect::LspImplementationRequest,
        ),
        lsp_references_request: register_simple(
            registry,
            "action:lsp-references",
            "`gr`: send `textDocument/references` to attached LSP servers.",
            AppEffect::LspReferencesRequest,
        ),
        enter_append: register_simple(
            registry,
            "action:enter-append",
            "Vim's `a`: move right one byte and enter Insert.",
            AppEffect::EnterAppend,
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
        snippet_expand: register_simple(
            registry,
            "action:snippet-expand",
            "Insert mode's `<C-x><C-s>`: direct snippet expansion.",
            AppEffect::SnippetExpand,
        ),
    }
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
            apply: Box::new(move |_ctx| {
                Ok(lattice_grammar::Effect::AppAction(effect.clone()))
            }),
            args_schema: vec![],
        },
    )
}

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
        let ids = populate(&mut registry);
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
            (ids.walk_mark_history_forward, "action:walk-mark-history-forward"),
            (ids.tag_stack_pop, "action:tag-stack-pop"),
            (ids.open_fold_at_cursor, "action:open-fold-at-cursor"),
            (ids.close_fold_at_cursor, "action:close-fold-at-cursor"),
            (ids.toggle_fold_at_cursor, "action:toggle-fold-at-cursor"),
            (ids.open_all_folds, "action:open-all-folds"),
            (ids.close_all_folds, "action:close-all-folds"),
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
            (ids.enter_command_line, "action:enter-command-line"),
            (ids.oil_navigate_up, "action:oil-navigate-up"),
            (ids.reselect_last_visual, "action:reselect-last-visual"),
            (ids.paste_after, "action:paste-after"),
            (ids.paste_before, "action:paste-before"),
            (ids.lsp_definition_request, "action:lsp-definition"),
            (ids.lsp_declaration_request, "action:lsp-declaration"),
            (ids.lsp_type_definition_request, "action:lsp-type-definition"),
            (ids.lsp_implementation_request, "action:lsp-implementation"),
            (ids.lsp_references_request, "action:lsp-references"),
            (ids.enter_append, "action:enter-append"),
            (ids.create_fold_from_visual, "action:create-fold-from-visual"),
            (ids.delete_char_backward, "action:delete-char-backward"),
            (ids.completion_trigger, "action:completion-trigger"),
            (ids.snippet_expand, "action:snippet-expand"),
        ] {
            let spec = registry.lookup(id).unwrap_or_else(|| {
                panic!("missing registry entry for `{expected_name}`")
            });
            assert_eq!(spec.name, expected_name);
        }
    }

    #[test]
    fn dispatch_returns_app_action_effect() {
        let mut registry = CommandRegistry::new();
        let ids = populate(&mut registry);
        let mut doc = lattice_core::Document::empty();
        let inv = CommandInvocation::of(ids.match_bracket);
        let eff = execute(
            &registry,
            &mut doc,
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
}
