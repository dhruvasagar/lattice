//! `ai-permission-mode` — the ACP permission menu (PU-B).
//!
//! A momentary popup surface over a dynamic option list (design §5.3). The
//! buffer is a `BufferData::Help` popup (so the popup renderer draws it) whose
//! MAJOR mode is this one: the KIND names the surface, this mode names the
//! behaviour. `on_activate` reads the oldest `Pending` permission from the
//! [`ConversationStore`] and owner-writes the projection; `<CR>` on an option
//! line resolves it (the file-tree / oil `entry_at_line` model), `Esc`/`q`
//! defer. Resolution routes through
//! [`ConversationStore::resolve_permission`](crate::acp::conversation::ConversationStore::resolve_permission)
//! by the agent's `option_id` (PU-B.2a).
//!
//! v1 uses `<CR>`-by-cursor as the selector. The `1`–`9` digit accelerators the
//! design also calls for need a count-override seam (bare digits are parsed as
//! vim counts, `Action::PushDigit`, before any mode chord lookup), which no mode
//! has today — deferred to a follow-up.

use std::sync::{Arc, Mutex, OnceLock};

use agent_client_protocol::schema::v1::PermissionOptionId;
use lattice_grammar::effect::Effect;
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle, CapabilitySet,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};

use crate::acp::conversation::{ConversationStore, PendingPermissionView};

/// The synthetic buffer name the popup menu is projected into. `:ai-permission`
/// and the (PU-B.3) auto-opener both name it in `Effect::OpenPopup`.
pub const PERMISSION_BUFFER_NAME: &str = "*ai-permission*";

/// Projection layout: `title` then a blank line, so the first option line is
/// row 2. `<CR>`-by-cursor maps `cursor.line - FIRST_OPTION_LINE` → option index.
/// A `description`, when present, is rendered AFTER the options so this offset
/// stays fixed.
const FIRST_OPTION_LINE: u32 = 2;

/// Menu state populated by `on_activate`, read by the select handler: the
/// request id to resolve and the option ids in wire order (index → option).
#[derive(Default)]
struct MenuState {
    request_id: Option<String>,
    option_ids: Vec<PermissionOptionId>,
}

/// `ai-permission-mode`: the major mode of the `*ai-permission*` popup buffer.
#[derive(Clone, Default)]
pub struct AiPermissionMode {
    menu: Arc<Mutex<MenuState>>,
}

impl AiPermissionMode {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn mode_id() -> ModeId {
        ModeId::new("ai-permission-mode")
    }
}

impl Mode for AiPermissionMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    /// The menu is a read-only, non-file surface — Insert / operators never edit
    /// it; the projection is owner-written in `on_activate`.
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(ai_permission_keymap_entries())
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            ActionHandlerContribution {
                action_name: "action:ai-perm-select",
                handler: select_at_cursor_handler(self.menu.clone()),
            },
            ActionHandlerContribution {
                action_name: "action:ai-perm-dismiss",
                handler: dismiss_handler(),
            },
        ]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        let menu = self.menu.clone();
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(());
            };
            let Some(conv_store) = ctx.service::<ConversationStore>() else {
                return Ok(());
            };
            // Each open is a fresh buffer (dismiss tears the prior one down), so
            // projecting the CURRENT oldest-pending request on every activation
            // keeps the menu in sync as requests resolve.
            let (text, state) = match conv_store.oldest_pending_permission() {
                Some(pending) => {
                    let state = MenuState {
                        request_id: Some(pending.id.clone()),
                        option_ids: pending.options.iter().map(|o| o.option_id.clone()).collect(),
                    };
                    (project_permission(&pending), state)
                }
                None => (
                    "No pending permission request.\n\n  Esc  close\n".to_string(),
                    MenuState::default(),
                ),
            };
            full_replace(&handle, &text).await;
            *menu.lock().expect("permission menu mutex poisoned") = state;
            Ok(())
        })
    }
}

/// Project the request into the popup buffer (design §5.3):
/// ```text
/// {title}
///
///   1  {option name}
///   2  {option name}
///
///   Esc  decide later
/// ```
/// A `description`, when present, follows the options so `FIRST_OPTION_LINE`
/// stays fixed for `<CR>`-by-cursor.
fn project_permission(p: &PendingPermissionView) -> String {
    let mut out = String::new();
    out.push_str(&p.title);
    out.push('\n');
    out.push('\n');
    for (i, opt) in p.options.iter().enumerate() {
        out.push_str(&format!("  {}  {}\n", i + 1, opt.name));
    }
    out.push('\n');
    if let Some(desc) = &p.description {
        out.push_str(desc);
        out.push('\n');
        out.push('\n');
    }
    out.push_str("  Esc  decide later\n");
    out
}

fn ai_permission_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! {
                mode: Normal, chord: "<CR>",
                doc: "ai-permission: select the option under the cursor",
                cmd: "action:ai-perm-select"
            },
            keymap_entry! {
                mode: Normal, chord: "<Esc>",
                doc: "ai-permission: defer the request (leave it pending)",
                cmd: "action:ai-perm-dismiss"
            },
            keymap_entry! {
                mode: Normal, chord: "q",
                doc: "ai-permission: defer the request (leave it pending)",
                cmd: "action:ai-perm-dismiss"
            },
        ]
    })
}

/// `<CR>`: resolve the option on the cursor line (file-tree / oil
/// `entry_at_line` model). Lines outside the option range (title, blanks, the
/// `Esc` hint) map to no option, so the keystroke is a harmless no-op there.
fn select_at_cursor_handler(menu: Arc<Mutex<MenuState>>) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let (id, option_id) = {
            let guard = menu.lock().ok()?;
            let id = guard.request_id.clone()?;
            let index = ctx.cursor.line.checked_sub(FIRST_OPTION_LINE)? as usize;
            (id, guard.option_ids.get(index)?.clone())
        };
        let store = ctx.services.get::<ConversationStore>()?;
        store.resolve_permission(&id, option_id);
        // A choice was made — close the menu.
        Some(Effect::DismissPopup)
    })
}

/// `Esc`/`q`: dismiss the popup WITHOUT resolving — the request stays `Pending`
/// (the inline block keeps rendering it; `:ai-permission` reopens the menu).
fn dismiss_handler() -> ActionHandler {
    Arc::new(|_ctx: &ActionContext<'_>| -> Option<Effect> { Some(Effect::DismissPopup) })
}

/// Register the `ai-permission` action commands so the mode's keymap `cmd`
/// names resolve at boot (the `register_ai_conversation_actions` pattern). The
/// specs are pure shells returning `Effect::None`; the real bodies live in
/// [`AiPermissionMode::action_handlers`], consulted before the CommandSpec.
pub fn register_ai_permission_actions(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::registry::ActionSpec;
    for (name, doc) in [
        (
            "action:ai-perm-select",
            "ai-permission: select the option under the cursor.",
        ),
        (
            "action:ai-perm-dismiss",
            "ai-permission: defer the request (leave it pending).",
        ),
    ] {
        registry.register_action(
            name,
            doc,
            ActionSpec {
                apply: Box::new(|_| Ok(Effect::None)),
                args_schema: vec![],
            },
        );
    }
}

/// Owner-write the whole buffer to `text` (a single full-range replace). The
/// menu buffer is read-only to the user, so this bypasses the modal edit gate
/// the same way the conversation drain seeds its transcript.
async fn full_replace(handle: &std::sync::Arc<dyn lattice_runtime::Document>, text: &str) {
    let snap = handle.snapshot();
    let last_line = snap.buffer.line_count().saturating_sub(1);
    let last_len = snap.buffer.line(last_line).unwrap_or_default().len() as u32;
    let range = lattice_protocol::Range::new(
        lattice_protocol::position::Position::new(0, 0),
        lattice_protocol::position::Position::new(last_line, last_len),
    );
    let edit = lattice_protocol::edit::Edit::replace(range, text.to_string());
    let _ = handle.apply_edit_batch(vec![edit]).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol::schema::v1::{PermissionOption, PermissionOptionKind};

    fn view() -> PendingPermissionView {
        PendingPermissionView {
            id: "perm-1".to_string(),
            title: "Allow `cargo test`?".to_string(),
            description: None,
            options: vec![
                PermissionOption::new("allow-once", "Allow once", PermissionOptionKind::AllowOnce),
                PermissionOption::new("reject-once", "Reject", PermissionOptionKind::RejectOnce),
            ],
        }
    }

    /// The projection's option rows MUST start at `FIRST_OPTION_LINE`, because
    /// the `<CR>`-by-cursor handler maps `cursor.line - FIRST_OPTION_LINE` to the
    /// option index. This pins the layout↔handler contract.
    #[test]
    fn projection_places_options_at_first_option_line() {
        let text = project_permission(&view());
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Allow `cargo test`?");
        assert_eq!(lines[1], "");
        assert_eq!(lines[FIRST_OPTION_LINE as usize], "  1  Allow once");
        assert_eq!(lines[FIRST_OPTION_LINE as usize + 1], "  2  Reject");
        assert!(text.contains("Esc  decide later"));
    }

    /// A description is rendered AFTER the options, so it never shifts
    /// `FIRST_OPTION_LINE` and the cursor→index mapping stays correct.
    #[test]
    fn projection_renders_description_after_options() {
        let mut v = view();
        v.description = Some("Runs an arbitrary shell command.".to_string());
        let text = project_permission(&v);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[FIRST_OPTION_LINE as usize], "  1  Allow once");
        assert!(text.contains("Runs an arbitrary shell command."));
    }
}
