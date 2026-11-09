//! Inline output: `destination=below` (LL.13b, design `llm-layer.md` §5, §7, §9).
//!
//! The answer to an ask streams into the source buffer itself, on new lines
//! under the text it is about. The request is an ordinary hidden chat session,
//! so settings, keys and the system prompt resolve exactly as for a chat, and
//! the full reply can still be seen and followed up as one; the session's
//! [`TextTap`](super::chat::TextTap) hands each piece of text to a writer here.
//!
//! **One tracked range per answer**, empty at the end of the span's last line
//! and `NeverGrows`, so text typed at its edges stays outside it. Every write
//! is [`append_to_tracked`](lattice_runtime::Document::append_to_tracked),
//! which resolves the range in the document actor's turn: the answer stays one
//! contiguous span whatever the user types around it. The first write opens it
//! with a newline, so a partial line never runs into the line below; trailing
//! newlines are held back until more text follows, so the answer ends where
//! the line it was written above begins, and leading ones are dropped.
//!
//! **Pacing.** Text coalesces to at most one write per frame interval
//! ([`FRAME`]): only the answer's tail changes on screen, once a frame, and the
//! editor is woken after each write so it shows without a keystroke. The host
//! adopts the caret an append moved on that wake, so a caret at the end of the
//! answer follows it and the view with it; a caret anywhere else stays on its
//! text.
//!
//! **Stopping.** The write is refused, and the request cancelled, when the
//! range's text was deleted ([`AppendOutcome::Collapsed`]) or the user pressed
//! `u` or `<C-r>` since the last write ([`AppendOutcome::Undone`]): what was
//! written stays (an undo has already taken its share), and the echo says why.
//! A failure partway leaves the partial text and a visible `[interrupted]`
//! marker; a failure before any text leaves nothing.
//!
//! **The quiet destinations (LL.14a).** `echo` and `register` wait for the
//! whole reply, then hand it over in one effect: the echo area shows its first
//! line (the echo area is one line, and a reply collapsed into one is
//! unreadable), a register takes all of it. Either way the reply stays in the
//! ask's hidden session, which `:llm-last` opens.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use lattice_core::BufferId;
use lattice_core::tracked::{AppendOutcome, Stickiness, TrackedRangeChange, TrackedRangeId};
use lattice_grammar::Register;
use lattice_grammar::effect::{EchoLevel, Effect, YankKind};
use lattice_mode::BufferStoreHandle;
use lattice_mode::inbound::InboundBus;
use lattice_protocol::position::Position;
use tokio::runtime::Handle;
use tokio::sync::Notify;

use super::chat::{ChatSession, LlmChats};
use super::operators::{AskSource, echo};
use super::settings::RequestOverrides;
use crate::conversation::Block;

/// The shortest gap between two writes into the buffer: one frame at 60 Hz.
pub const FRAME: Duration = Duration::from_millis(16);

/// The line an interrupted answer ends with.
pub const INTERRUPTED_MARKER: &str = "[interrupted]";

/// Effects for the editor, sent from a writer task. An empty batch is a wake:
/// the editor repaints and adopts the caret the last write moved.
pub struct InlineEffects(pub Vec<Effect>);

/// The answers streaming into buffers now.
pub struct LlmInline {
    runtime: Handle,
    live: Mutex<Vec<(BufferId, TrackedRangeId)>>,
    effects: Mutex<Option<InboundBus<InlineEffects>>>,
}

pub type LlmInlineHandle = Arc<LlmInline>;

impl LlmInline {
    /// The multi-thread runtime the layer's background work runs on.
    ///
    /// Not `Handle::try_current()` inside a mode's `on_activate`: activation is
    /// polled on the editor thread first, so that returns the editor's
    /// `current_thread` runtime, and work spawned there runs beside keystrokes.
    pub fn runtime(&self) -> &Handle {
        &self.runtime
    }

    pub fn new(runtime: Handle) -> Self {
        Self {
            runtime,
            live: Mutex::new(Vec::new()),
            effects: Mutex::new(None),
        }
    }

    /// The bus a writer's effects reach the editor through. Without one (a
    /// harness with no host), nothing is woken and echoes are dropped.
    pub fn set_effects(&self, bus: InboundBus<InlineEffects>) {
        *self.effects.lock().unwrap_or_else(|e| e.into_inner()) = Some(bus);
    }

    /// How many answers are streaming.
    pub fn live_count(&self) -> usize {
        self.live.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Wake the editor with no effects: something it shows changed off a
    /// keystroke (the requests list was rewritten).
    pub fn wake(&self) {
        self.send(Vec::new());
    }

    /// Hand `effects` to the editor and wake it.
    pub(crate) fn send(&self, effects: Vec<Effect>) {
        if let Some(bus) = self
            .effects
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            let _ = bus.send(InlineEffects(effects));
        }
    }

    fn forget(&self, id: TrackedRangeId) {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(_, live)| *live != id);
    }

    /// Whether an answer is already streaming at byte `point` of `buffer`:
    /// at its insertion point, or inside the text it has written.
    fn busy_at(
        &self,
        buffer: BufferId,
        snap: &lattice_runtime::DocumentSnapshot,
        point: usize,
    ) -> bool {
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(b, _)| *b == buffer)
            .filter_map(|(_, id)| snap.tracked_ranges.get(*id))
            .any(|r| r.start <= point && point <= r.end)
    }

    /// Stream the answer to `block` + `instruction` into `source`'s buffer,
    /// under the span's last line. The caller has resolved the destination,
    /// refused read-only and composed buffers, and folded the request's
    /// overrides; `hint` names the hidden session.
    #[allow(clippy::too_many_arguments)]
    pub fn start(
        self: &Arc<Self>,
        chats: &Arc<LlmChats>,
        store: &BufferStoreHandle,
        source: &AskSource,
        hint: &str,
        block: Block,
        instruction: String,
        overrides: RequestOverrides,
    ) -> Effect {
        let Some(handle) = store.handle_for(source.buffer) else {
            return echo(
                EchoLevel::Error,
                "llm: the source buffer is gone".to_string(),
            );
        };
        let snap = handle.snapshot();
        let Some(point) = insertion_point(&snap.buffer, source) else {
            return echo(
                EchoLevel::Error,
                "llm: the text asked about is no longer there".to_string(),
            );
        };
        if self.busy_at(source.buffer, &snap, point) {
            return echo(
                EchoLevel::Error,
                "llm: an answer is already streaming here; wait for it, or `u` to stop it"
                    .to_string(),
            );
        }
        let id = TrackedRangeId::next();
        // Sent before the writer exists, so the mailbox has it ahead of any
        // write; nothing to wait for.
        drop(
            handle.update_tracked_ranges(vec![TrackedRangeChange::Track {
                id,
                bytes: point..point,
                stickiness: Stickiness::NeverGrows,
            }]),
        );
        self.live
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((source.buffer, id));

        let session = chats.create_for_source(source.buffer, hint);
        session.set_destination(format!("below {}", block_label(&block, hint)));
        let pending = Arc::new(Pending::default());
        {
            let pending = pending.clone();
            session.set_text_tap(Arc::new(move |t: &str| pending.push(t)));
        }
        let ended = session.wait_turn_end();
        chats.submit_source(&session, block, instruction, overrides);
        let writer = Writer {
            inline: self.clone(),
            chats: chats.clone(),
            store: store.clone(),
            session,
            buffer: source.buffer,
            id,
            pending,
        };
        self.runtime.spawn(writer.run(ended));
        echo(EchoLevel::Info, "llm: answering below".to_string())
    }
}

/// Where a quiet answer goes once it is complete (LL.14a).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quiet {
    /// The echo area: the reply's first line, and how much more there is.
    Echo,
    /// A register: the whole reply, ready to put.
    Register(Register),
}

impl LlmInline {
    /// Send `block` + `instruction` from `buffer` in a hidden session and hand
    /// the complete reply to `quiet`. Returns the echo saying it was sent.
    #[allow(clippy::too_many_arguments)]
    pub fn start_quiet(
        self: &Arc<Self>,
        chats: &Arc<LlmChats>,
        buffer: BufferId,
        hint: &str,
        block: Block,
        instruction: String,
        overrides: RequestOverrides,
        quiet: Quiet,
    ) -> Effect {
        let session = chats.create_for_source(buffer, hint);
        session.set_destination(match quiet {
            Quiet::Echo => format!("echo, from {}", block_label(&block, hint)),
            Quiet::Register(r) => {
                format!("{}, from {}", register_name(r), block_label(&block, hint))
            }
        });
        let ended = session.wait_turn_end();
        chats.submit_source(&session, block, instruction, overrides);
        let inline = self.clone();
        self.runtime.spawn(async move {
            let result = ended
                .await
                .unwrap_or_else(|_| Err("the request was dropped".to_string()));
            inline.send(quiet_effects(quiet, result));
        });
        let text = match quiet {
            Quiet::Echo => "llm: asking; the answer will show here".to_string(),
            Quiet::Register(r) => {
                format!("llm: asking; the answer will go in {}", register_name(r))
            }
        };
        echo(EchoLevel::Info, text)
    }
}

/// What a source block is called in listings: `foo.rs:4-6`.
fn block_label<'a>(block: &'a Block, hint: &'a str) -> &'a str {
    match block {
        Block::Source { label, .. } => label,
        _ => hint,
    }
}

/// How a register is named to the user: `"a`, or the unnamed register.
fn register_name(r: Register) -> String {
    match r {
        Register::Named(c) | Register::ReadOnly(c) => format!("register \"{c}"),
        Register::Numbered(n) => format!("register \"{n}"),
        Register::System => "register \"+".to_string(),
        Register::Unnamed => "the unnamed register".to_string(),
        Register::BlackHole => "register \"_".to_string(),
        Register::Expression => "register \"=".to_string(),
    }
}

/// What a finished quiet answer does: the echo or the register write, or the
/// echo explaining why there is nothing.
fn quiet_effects(quiet: Quiet, result: Result<String, String>) -> Vec<Effect> {
    let reply = match result {
        Ok(reply) => reply,
        Err(e) if e == "interrupted" => {
            return vec![echo(
                EchoLevel::Info,
                "llm: the answer was stopped".to_string(),
            )];
        }
        Err(e) => return vec![echo(EchoLevel::Error, format!("llm: {e}"))],
    };
    let reply = reply.trim_matches(['\n', '\r']);
    if reply.trim().is_empty() {
        return vec![echo(
            EchoLevel::Info,
            "llm: the model answered with nothing".to_string(),
        )];
    }
    let lines = reply.lines().count();
    match quiet {
        Quiet::Echo => {
            let first = reply.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            let text = if lines > 1 {
                format!(
                    "{} (+{} lines; :llm-last shows all)",
                    first.trim(),
                    lines - 1
                )
            } else {
                first.trim().to_string()
            };
            vec![echo(EchoLevel::Info, text)]
        }
        Quiet::Register(register) => {
            // Several lines put as lines (`p` opens them below the caret, as
            // a code block wants); one line puts into the text.
            let (content, kind) = if lines > 1 {
                (format!("{reply}\n"), YankKind::Linewise)
            } else {
                (reply.to_string(), YankKind::Charwise)
            };
            let noun = if lines == 1 { "line" } else { "lines" };
            vec![
                Effect::Yank {
                    register,
                    content,
                    kind,
                    explicit_yank: false,
                },
                echo(
                    EchoLevel::Info,
                    format!(
                        "llm: the answer ({lines} {noun}) is in {}",
                        register_name(register)
                    ),
                ),
            ]
        }
    }
}

/// The byte at the end of the last line `source`'s span covers, before that
/// line's newline. A linewise span ends at the start of the line after it, so
/// that line is not counted.
fn insertion_point(buffer: &lattice_core::Buffer, source: &AskSource) -> Option<usize> {
    let (start, end) = (source.range.start, source.range.end);
    let last = if end.byte == 0 && end.line > start.line {
        end.line - 1
    } else {
        end.line
    };
    let len = buffer.line(last)?.len();
    buffer
        .position_to_byte(Position::new(last, len as u32))
        .ok()
}

/// Reply text the writer has not written yet, filled from the request task.
#[derive(Default)]
struct Pending {
    text: Mutex<String>,
    wake: Notify,
}

impl Pending {
    fn push(&self, t: &str) {
        self.text
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push_str(t);
        self.wake.notify_one();
    }

    /// Everything but trailing newlines, which stay until text follows them.
    fn take_ready(&self) -> String {
        let mut text = self.text.lock().unwrap_or_else(|e| e.into_inner());
        let ready_len = text.trim_end_matches(['\n', '\r']).len();
        let rest = text.split_off(ready_len);
        std::mem::replace(&mut *text, rest)
    }
}

/// Why a writer stopped writing before the reply ended.
enum Stop {
    Collapsed,
    Undone,
    Gone,
}

/// One answer, from the first piece of text to the last.
struct Writer {
    inline: Arc<LlmInline>,
    chats: Arc<LlmChats>,
    store: BufferStoreHandle,
    session: Arc<ChatSession>,
    buffer: BufferId,
    id: TrackedRangeId,
    pending: Arc<Pending>,
}

impl Writer {
    async fn run(self, ended: tokio::sync::oneshot::Receiver<Result<String, String>>) {
        tokio::pin!(ended);
        let mut outcome: Option<Result<String, String>> = None;
        let mut wrote = false;
        let mut last_write: Option<tokio::time::Instant> = None;
        loop {
            if outcome.is_none() {
                tokio::select! {
                    () = self.pending.wake.notified() => {}
                    r = &mut ended => {
                        outcome = Some(r.unwrap_or_else(|_| Err("the request was dropped".to_string())));
                    }
                }
            }
            if let Some(at) = last_write {
                tokio::time::sleep_until(at + FRAME).await;
            }
            let mut chunk = self.pending.take_ready();
            if !wrote {
                chunk = chunk.trim_start_matches(['\n', '\r']).to_string();
                if !chunk.is_empty() {
                    chunk.insert(0, '\n');
                }
            }
            if !chunk.is_empty() {
                match self.write(chunk).await {
                    Ok(()) => {
                        wrote = true;
                        last_write = Some(tokio::time::Instant::now());
                        self.inline.send(Vec::new());
                    }
                    Err(stop) => {
                        self.stopped(stop);
                        return;
                    }
                }
            }
            if let Some(result) = outcome.take() {
                // The reply's last text arrived before the turn ended; once it
                // is written (anything left is trailing newlines), finish.
                self.finish(result, wrote).await;
                return;
            }
        }
    }

    async fn write(&self, text: String) -> Result<(), Stop> {
        let Some(handle) = self.store.handle_for(self.buffer) else {
            return Err(Stop::Gone);
        };
        match handle.append_to_tracked(self.id, text).await {
            Ok(AppendOutcome::Appended) => Ok(()),
            Ok(AppendOutcome::Collapsed) => Err(Stop::Collapsed),
            Ok(AppendOutcome::Undone) => Err(Stop::Undone),
            Ok(AppendOutcome::Unknown) | Err(_) => Err(Stop::Gone),
        }
    }

    fn untrack(&self) {
        if let Some(handle) = self.store.handle_for(self.buffer) {
            drop(handle.update_tracked_ranges(vec![TrackedRangeChange::Untrack(self.id)]));
        }
        self.inline.forget(self.id);
    }

    fn stopped(&self, stop: Stop) {
        self.chats.interrupt(&self.session);
        self.untrack();
        let text = match stop {
            Stop::Collapsed => {
                "llm: the text the answer was going under was deleted; stopped".to_string()
            }
            Stop::Undone => "llm: stopped by undo".to_string(),
            Stop::Gone => "llm: the buffer the answer was going into is gone; stopped".to_string(),
        };
        self.inline.send(vec![echo(EchoLevel::Info, text)]);
    }

    async fn finish(&self, result: Result<String, String>, wrote: bool) {
        let effects = match result {
            Ok(_) if wrote => Vec::new(),
            Ok(_) => vec![echo(
                EchoLevel::Info,
                "llm: the model answered with nothing".to_string(),
            )],
            Err(e) if wrote => {
                // Part of the answer's span, so the `u` that removes the
                // answer removes the marker with it. A refused marker (the
                // text was deleted meanwhile) needs no mention.
                let _ = self.write(format!("\n{INTERRUPTED_MARKER}")).await;
                let why = if e == "interrupted" {
                    "llm: the answer was stopped".to_string()
                } else {
                    format!("llm: the answer was interrupted ({e})")
                };
                vec![echo(EchoLevel::Error, why)]
            }
            Err(e) if e == "interrupted" => vec![echo(
                EchoLevel::Info,
                "llm: stopped; nothing was written".to_string(),
            )],
            Err(e) => vec![echo(EchoLevel::Error, format!("llm: {e}"))],
        };
        self.untrack();
        self.inline.send(effects);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_protocol::position::Range;

    fn point(buffer: &lattice_core::Buffer, start: (u32, u32), end: (u32, u32)) -> Option<usize> {
        let src = AskSource {
            buffer: BufferId(1),
            range: Range::new(Position::new(start.0, start.1), Position::new(end.0, end.1)),
            text: String::new(),
            register: Register::Unnamed,
        };
        insertion_point(buffer, &src)
    }

    #[test]
    fn the_answer_goes_at_the_end_of_the_spans_last_line() {
        let b = lattice_core::Buffer::from_text("alpha\nbeta gamma\ndelta\n");
        assert_eq!(
            point(&b, (1, 0), (1, 4)),
            Some(16),
            "charwise: the end of `beta gamma`"
        );
        assert_eq!(
            point(&b, (0, 0), (2, 0)),
            Some(16),
            "linewise: the line before"
        );
        assert_eq!(point(&b, (2, 0), (2, 5)), Some(22));
    }

    #[test]
    fn a_quiet_answer_echoes_its_first_line_and_puts_all_of_it() {
        let reply = Ok("\nFirst line.\nsecond\nthird\n".to_string());
        let echoed = quiet_effects(Quiet::Echo, reply.clone());
        assert!(matches!(&echoed[..], [Effect::Echo { text, .. }]
            if text == "First line. (+2 lines; :llm-last shows all)"));
        let put = quiet_effects(Quiet::Register(Register::Named('a')), reply);
        assert!(
            matches!(&put[0], Effect::Yank { register: Register::Named('a'), content, kind: YankKind::Linewise, explicit_yank: false }
            if content == "First line.\nsecond\nthird\n")
        );
        let one = quiet_effects(Quiet::Register(Register::Unnamed), Ok("42".to_string()));
        assert!(
            matches!(&one[0], Effect::Yank { content, kind: YankKind::Charwise, .. } if content == "42")
        );
        assert!(
            matches!(&one[1], Effect::Echo { text, .. } if text.contains("the unnamed register"))
        );
    }

    #[test]
    fn a_quiet_answer_that_fails_or_is_empty_only_echoes() {
        for (result, want) in [
            (Err("overloaded".to_string()), "llm: overloaded"),
            (
                Err("interrupted".to_string()),
                "llm: the answer was stopped",
            ),
            (
                Ok(" \n".to_string()),
                "llm: the model answered with nothing",
            ),
        ] {
            let e = quiet_effects(Quiet::Register(Register::Unnamed), result);
            assert!(
                matches!(&e[..], [Effect::Echo { text, .. }] if text == want),
                "{want}"
            );
        }
    }

    #[test]
    fn trailing_newlines_wait_for_text_after_them() {
        let p = Pending::default();
        p.push("one\n\n");
        assert_eq!(p.take_ready(), "one");
        assert_eq!(p.take_ready(), "");
        p.push("two");
        assert_eq!(p.take_ready(), "\n\ntwo");
    }
}
