//! `llm-context-mode` and `llm-inspect-mode`: what a request will carry, and
//! exactly what one sends (LL.15, design `llm-layer.md` §5).
//!
//! `*llm-context*` lists the context of the buffer `:llm-context` ran in:
//! each attached buffer, span and file, with where it stands now (a span's
//! current lines, a closed buffer). `dd` on a row removes that item.
//! `*llm-inspect*` shows a request as it goes over the wire, key redacted.
//!
//! **Both keep themselves current.** Each subscribes, while open, to a
//! counter the llm layer bumps on every change ([`crate::llm::context::Watch`]),
//! rewrites its text when it differs, and wakes the editor, so attaching from
//! another window, or an inspect finishing its build off the editor thread,
//! shows without a keystroke.
//!
//! Read-only by the two declarations: `ReadOnly = true` stops typing,
//! `read-only-mode` (implied) refuses operators. `dd` in the list is this
//! mode's chord, at the major-mode layer, so it shadows the operator.

use std::sync::{Arc, OnceLock};

use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle, CapabilitySet,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_runtime::Document;

use crate::llm::chat::LlmChatsHandle;
use crate::llm::context::{Row, WatchHandle, name_for, rows};
use crate::llm::modes::requests::replace_text;
use crate::llm::sink::LlmInlineHandle;

/// The context list's buffer: one, showing whichever buffer's context
/// `:llm-context` last ran in.
pub const CONTEXT_BUFFER: &str = "*llm-context*";

/// The inspect view's buffer.
pub const INSPECT_BUFFER: &str = "*llm-inspect*";

/// `dd` in the list: remove the item on the caret's row.
pub const ACTION_REMOVE: &str = "action:llm-context-remove";

/// The rows above the first item in the list.
const HEADER_LINES: u32 = 1;

/// Stops a view's refresher when its buffer closes or the mode is replaced.
pub struct RefreshGuard {
    task: tokio::task::AbortHandle,
}

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Keep `buffer`'s text equal to `text()`: now, and after every bump of
/// `watch`, waking the editor when it changed.
fn refresh(
    ctx: &ModeContext,
    watch: &WatchHandle,
    inline: LlmInlineHandle,
    text: impl Fn(&BufferStoreHandle) -> String + Send + 'static,
) -> Option<RefreshGuard> {
    let buffer = lattice_core::BufferId(ctx.buffer_id().0 as u32);
    let store = ctx.service::<BufferStoreHandle>()?;
    let handle: Arc<dyn Document> = store.handle_for(buffer)?;
    let runtime = tokio::runtime::Handle::try_current().ok()?;
    // Subscribed before the first paint, so a change between the two is not
    // missed: at worst it repaints the same text, which is skipped.
    let mut rx = watch.subscribe();
    let task = runtime.spawn(async move {
        loop {
            if replace_text(&handle, text(&store)).await {
                inline.wake();
            }
            if rx.changed().await.is_err() {
                return;
            }
        }
    });
    Some(RefreshGuard {
        task: task.abort_handle(),
    })
}

fn read_only_options() -> OptionOverrideSet {
    lattice_config::overrides! {
        lattice_config::ReadOnly = true,
        lattice_config::NoFile = true,
    }
}

/// The second declaration: `ReadOnly` gates typing only; `read-only-mode`
/// carries the runner that refuses `x`, `p` and the other operators.
fn read_only_implied() -> &'static [ModeId] {
    static IMPLIED: OnceLock<Vec<ModeId>> = OnceLock::new();
    IMPLIED.get_or_init(|| vec![lattice_mode::modes::ReadOnlyMode::mode_id()])
}

pub struct LlmContextMode {
    chats: LlmChatsHandle,
    inline: LlmInlineHandle,
}

impl LlmContextMode {
    pub fn new(chats: LlmChatsHandle, inline: LlmInlineHandle) -> Self {
        Self { chats, inline }
    }

    pub fn mode_id() -> ModeId {
        ModeId::new("llm-context-mode")
    }
}

impl Mode for LlmContextMode {
    type Guard = Option<RefreshGuard>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn options(&self) -> OptionOverrideSet {
        read_only_options()
    }

    fn implies(&self) -> &[ModeId] {
        read_only_implied()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
        Keymap::from_entries(ENTRIES.get_or_init(|| {
            vec![keymap_entry! {
                mode: Normal, chord: "dd",
                doc: "llm-context: remove the item on this line",
                cmd: "action:llm-context-remove"
            }]
        }))
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![ActionHandlerContribution {
            action_name: ACTION_REMOVE,
            handler: remove_handler(self.chats.clone()),
        }]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        let chats = self.chats.clone();
        let guard = refresh(
            &ctx,
            self.chats.context_watch(),
            self.inline.clone(),
            move |store| render_list(&chats, store),
        );
        Box::pin(async move { Ok(guard) })
    }
}

/// The list's text: whose context, then a row per item.
pub fn render_list(chats: &LlmChatsHandle, store: &BufferStoreHandle) -> String {
    let Some(owner) = chats.listing_owner() else {
        return "Context: run :llm-context in a buffer to see what its requests carry\n"
            .to_string();
    };
    let whose = name_for(store, owner);
    let mut out = if chats.for_buffer(owner).is_some() {
        format!("Context for {whose}: read and sent with each of its requests\n")
    } else {
        format!("Context for {whose}: read and sent with each request made from it\n")
    };
    out.push_str(&format_rows(&rows(&chats.context_of(owner), store)));
    out
}

/// The rows, aligned. Pure, for tests.
pub fn format_rows(rows: &[Row]) -> String {
    if rows.is_empty() {
        return "  (nothing attached: <leader>ac{motion} attaches text, \
                :llm-context-add a buffer or a file)\n"
            .to_string();
    }
    let width = rows
        .iter()
        .map(|r| r.label.chars().count())
        .max()
        .unwrap_or(0);
    rows.iter()
        .map(|r| {
            let line = format!("  {:<6}  {:<width$}  {}", r.kind, r.label, r.status);
            format!("{}\n", line.trim_end())
        })
        .collect()
}

fn remove_handler(chats: LlmChatsHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let say = |text: String| {
            Some(Effect::Echo {
                level: EchoLevel::Info,
                text,
            })
        };
        let Some(owner) = chats.listing_owner() else {
            return say("llm: this list shows no buffer's context".to_string());
        };
        let Some(index) = ctx.cursor.line.checked_sub(HEADER_LINES) else {
            return say("llm: no context item on this line".to_string());
        };
        match chats.detach(owner, index as usize) {
            Some(removed) => say(format!("llm: removed {} from the context", removed.name)),
            None => say("llm: no context item on this line".to_string()),
        }
    })
}

/// Declare `dd`'s action so the keymap's `cmd` name resolves; the body is the
/// mode's.
pub fn register_actions(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::registry::ActionSpec;
    registry.register_action(
        ACTION_REMOVE,
        "llm-context: remove the item on this line from the context.",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
}

pub struct LlmInspectMode {
    chats: LlmChatsHandle,
    inline: LlmInlineHandle,
}

impl LlmInspectMode {
    pub fn new(chats: LlmChatsHandle, inline: LlmInlineHandle) -> Self {
        Self { chats, inline }
    }

    pub fn mode_id() -> ModeId {
        ModeId::new("llm-inspect-mode")
    }
}

impl Mode for LlmInspectMode {
    type Guard = Option<RefreshGuard>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn options(&self) -> OptionOverrideSet {
        read_only_options()
    }

    fn implies(&self) -> &[ModeId] {
        read_only_implied()
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        let chats = self.chats.clone();
        let guard = refresh(
            &ctx,
            self.chats.inspect_watch(),
            self.inline.clone(),
            move |_| chats.inspect_text(),
        );
        Box::pin(async move { Ok(guard) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_align_and_an_empty_list_says_how_to_attach() {
        let text = format_rows(&[
            Row {
                kind: "buffer",
                label: "main.rs".into(),
                status: "12 lines".into(),
            },
            Row {
                kind: "lines",
                label: "lib.rs:3-10".into(),
                status: String::new(),
            },
            Row {
                kind: "file",
                label: "notes.md".into(),
                status: "closed".into(),
            },
        ]);
        assert_eq!(
            text,
            "  buffer  main.rs      12 lines\n  lines   lib.rs:3-10\n  file    notes.md     closed\n"
        );
        assert!(format_rows(&[]).contains("<leader>ac{motion}"));
    }
}
