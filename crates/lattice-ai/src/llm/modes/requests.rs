//! `llm-requests-mode`: `*llm-requests*`, every LLM request running and the
//! last few finished (LL.14b, design `llm-layer.md` §5).
//!
//! A request outlives the surface that started it, so this is where all of
//! them are seen: running first, then finished, newest first, each with its
//! state, model and where its answer is going. `dd` on a row stops that
//! request, as `<C-c>` stops a chat's: through its session, so an inline
//! answer ends with its `[interrupted]` line and a quiet one says it stopped.
//!
//! **The list keeps itself current.** The mode subscribes to
//! [`RequestStateChanged`] while the buffer is open and rewrites the text
//! from the registry's snapshot on each change, then wakes the editor, so a
//! request finishing shows without a keystroke. The text is replaced only when
//! it differs. Usage updates publish nothing and are not shown.
//!
//! Read-only by the two declarations: `ReadOnly = true` stops typing,
//! `read-only-mode` (implied) refuses operators. `dd` is this mode's chord,
//! at the major-mode layer, so it shadows the operator.

use std::sync::{Arc, OnceLock};

use lattice_grammar::effect::{EchoLevel, Effect};
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, BufferStoreHandle, CapabilitySet,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
    keymap_entry,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::{Document, EventBus};

use crate::llm::chat::LlmChatsHandle;
use crate::llm::request::{RequestId, RequestInfo, RequestState, RequestStateChanged};
use crate::llm::sink::LlmInlineHandle;

/// The listing's buffer name: one list, reused by every `:llm-requests`.
pub const REQUESTS_BUFFER: &str = "*llm-requests*";

/// `dd`: stop the request on the caret's row.
pub const ACTION_STOP: &str = "action:llm-requests-stop";

pub struct LlmRequestsMode {
    chats: LlmChatsHandle,
    bus: Arc<EventBus>,
    inline: LlmInlineHandle,
}

impl LlmRequestsMode {
    pub fn new(chats: LlmChatsHandle, bus: Arc<EventBus>, inline: LlmInlineHandle) -> Self {
        Self { chats, bus, inline }
    }

    pub fn mode_id() -> ModeId {
        ModeId::new("llm-requests-mode")
    }
}

/// Stops the refresher when the buffer closes or the mode is replaced.
pub struct LlmRequestsGuard {
    task: tokio::task::AbortHandle,
}

impl Drop for LlmRequestsGuard {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Mode for LlmRequestsMode {
    type Guard = Option<LlmRequestsGuard>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }

    /// The second declaration: `ReadOnly` gates typing only; `read-only-mode`
    /// carries the runner that refuses `x`, `p` and the other operators.
    fn implies(&self) -> &[ModeId] {
        static IMPLIED: OnceLock<Vec<ModeId>> = OnceLock::new();
        IMPLIED.get_or_init(|| vec![lattice_mode::modes::ReadOnlyMode::mode_id()])
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(keymap_entries())
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![ActionHandlerContribution {
            action_name: ACTION_STOP,
            handler: stop_handler(self.chats.clone()),
        }]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer) else {
                return Ok(None);
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(None);
            };
            // Subscribed before the first paint, so a change between the two
            // is not missed: at worst it repaints the same text, which is
            // skipped.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<RequestStateChanged>();
            self.bus.subscribe_typed::<RequestStateChanged>(tx);
            let chats = self.chats.clone();
            let inline = self.inline.clone();
            paint(&handle, &chats, &inline).await;
            let task = runtime.spawn(async move {
                while rx.recv().await.is_some() {
                    // A burst of changes (a request starting is two) paints once.
                    while rx.try_recv().is_ok() {}
                    paint(&handle, &chats, &inline).await;
                }
            });
            Ok(Some(LlmRequestsGuard {
                task: task.abort_handle(),
            }))
        })
    }
}

fn keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![keymap_entry! {
            mode: Normal, chord: "dd",
            doc: "llm-requests: stop the request on this line",
            cmd: "action:llm-requests-stop"
        }]
    })
}

/// Rewrite the list from the registry if it changed, then wake the editor.
async fn paint(handle: &Arc<dyn Document>, chats: &LlmChatsHandle, inline: &LlmInlineHandle) {
    let text = render(&chats.requests().snapshot());
    let snap = handle.snapshot();
    if snap.buffer.as_string() == text {
        return;
    }
    let last = snap.buffer.rope_line_count().saturating_sub(1);
    let end = Position::new(
        last,
        snap.buffer.line(last).unwrap_or_default().len() as u32,
    );
    let _ = handle
        .apply_edit_batch(vec![Edit::replace(Range::new(Position::ZERO, end), text)])
        .await;
    inline.wake();
}

/// The listing's text: running requests in start order, then finished ones,
/// newest first. Pure, for tests.
pub fn render(requests: &[RequestInfo]) -> String {
    let running: Vec<_> = requests.iter().filter(|r| !r.state.is_terminal()).collect();
    let finished: Vec<_> = requests
        .iter()
        .rev()
        .filter(|r| r.state.is_terminal())
        .collect();
    let model_width = requests.iter().map(|r| r.model.len()).max().unwrap_or(0);
    let row = |r: &RequestInfo| {
        let id = format!("#{}", r.id.0);
        format!(
            "  {id:<5} {:<15} {:<model_width$}  {}\n",
            state_label(&r.state),
            r.model,
            r.target.label
        )
    };
    let mut out = String::from("Running\n");
    if running.is_empty() {
        out.push_str("  (none)\n");
    }
    for r in running {
        out.push_str(&row(r));
    }
    if !finished.is_empty() {
        out.push_str("\nFinished\n");
        for r in finished {
            out.push_str(&row(r));
            if let RequestState::Failed(why) = &r.state {
                out.push_str(&format!("        {}\n", why.lines().next().unwrap_or("")));
            }
        }
    }
    out
}

fn state_label(state: &RequestState) -> &'static str {
    match state {
        RequestState::Pending => "starting",
        RequestState::Streaming => "streaming",
        RequestState::AwaitingTool => "awaiting tool",
        RequestState::AwaitingReview => "awaiting review",
        RequestState::Done => "done",
        RequestState::Failed(_) => "failed",
        RequestState::Cancelled => "stopped",
    }
}

/// The request a listing row names, from its leading `#<id>`.
pub fn id_on(line: &str) -> Option<RequestId> {
    let digits: String = line
        .trim_start()
        .strip_prefix('#')?
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok().map(RequestId)
}

fn stop_handler(chats: LlmChatsHandle) -> ActionHandler {
    Arc::new(move |ctx: &ActionContext<'_>| -> Option<Effect> {
        let store = ctx.services.get::<BufferStoreHandle>()?;
        let handle = store.handle_for(lattice_core::BufferId(ctx.buffer_id.0 as u32))?;
        let line = handle.snapshot().buffer.line(ctx.cursor.line)?;
        let Some(id) = id_on(&line) else {
            return Some(say(EchoLevel::Info, "llm: no request on this line"));
        };
        let snapshot = chats.requests().snapshot();
        let Some(info) = snapshot.iter().find(|r| r.id == id) else {
            return Some(say(
                EchoLevel::Info,
                "llm: that request is no longer listed",
            ));
        };
        if info.state.is_terminal() {
            return Some(say(
                EchoLevel::Info,
                &format!("llm: request #{} has already finished", id.0),
            ));
        }
        chats.stop_request(id);
        Some(say(
            EchoLevel::Info,
            &format!("llm: stopping request #{} ({})", id.0, info.target.label),
        ))
    })
}

fn say(level: EchoLevel, text: &str) -> Effect {
    Effect::Echo {
        level,
        text: text.to_string(),
    }
}

/// Declare `dd`'s action so the keymap's `cmd` name resolves; the body is the
/// mode's.
pub fn register_actions(registry: &mut lattice_grammar::CommandRegistry) {
    use lattice_grammar::registry::ActionSpec;
    registry.register_action(
        ACTION_STOP,
        "llm-requests: stop the request on this line.",
        ActionSpec {
            apply: Arc::new(|_| Ok(Effect::None)),
            args_schema: vec![],
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::request::RequestTarget;
    use lattice_core::BufferId;

    fn info(id: u64, state: RequestState, label: &str) -> RequestInfo {
        RequestInfo {
            id: RequestId(id),
            target: RequestTarget {
                buffer: BufferId(1),
                label: label.to_string(),
            },
            model: "mock".to_string(),
            state,
            usage: None,
        }
    }

    #[test]
    fn running_come_first_then_finished_newest_first() {
        let text = render(&[
            info(1, RequestState::Done, "echo, from a.rs:1"),
            info(2, RequestState::Streaming, "below a.rs:3"),
            info(
                3,
                RequestState::Failed("overloaded\nmore".into()),
                "*llm:4*",
            ),
        ]);
        assert_eq!(
            text,
            "Running\n  #2    streaming       mock  below a.rs:3\n\nFinished\n  #3    failed          mock  *llm:4*\n        overloaded\n  #1    done            mock  echo, from a.rs:1\n"
        );
    }

    #[test]
    fn an_empty_list_says_nothing_is_running() {
        assert_eq!(render(&[]), "Running\n  (none)\n");
    }

    #[test]
    fn a_row_names_its_request_and_other_lines_do_not() {
        assert_eq!(id_on("  #12   streaming  m  x"), Some(RequestId(12)));
        assert_eq!(id_on("Running"), None);
        assert_eq!(id_on("        overloaded"), None);
        assert_eq!(id_on("  (none)"), None);
    }
}
