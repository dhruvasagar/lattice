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
