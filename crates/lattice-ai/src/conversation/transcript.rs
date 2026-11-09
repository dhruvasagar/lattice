//! A conversation buffer: a projected transcript with an editable prompt tail.
//!
//! Everything a transcript-with-a-prompt buffer needs that does not depend on
//! who produces the conversation: the prompt marker and per-buffer anchors, the
//! line-granular re-projection, the drain that keeps the buffer current, the
//! headerline / fold registrations and the generic prompt chords' bodies. The
//! ACP `ai-conversation` mode and the native `llm-chat` mode both bind a buffer
//! through [`bind`], so a fix to the drain or the guard lands in both.
//!
//! What stays in each mode: the chords' send / interrupt bodies (they talk to
//! different backends), the headerline's content, and the mode declaration.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use lattice_agent::SessionKey;
use lattice_cells::{Headerline, HeaderlineProvider, ProviderId, VirtualRowProvider};
use lattice_core::BufferId;
use lattice_grammar::effect::Effect;
use lattice_mode::{
    ActionContext, ActionHandler, EditableTail, ModeContext, ModeId, Subscription,
    VirtualRowRegistrar,
};

use crate::conversation::tool_fold::{
    FrameCell, ProjectionFrame, ReasoningFoldSource, ToolCallFoldSource,
};
use crate::conversation::{
    Conversation, ConversationProjected, ConversationStore, ConversationUpdated, ProjectionSource,
    project_conversation_with,
};

/// The shortest gap between two writes a stream makes into a buffer: one
/// frame at 60 Hz. A model streams tokens faster than the screen repaints;
/// writing each would re-project, re-highlight and repaint for frames nobody
/// sees. The chat drain and inline answers both pace on it.
pub const FRAME: std::time::Duration = std::time::Duration::from_millis(16);

/// Provider id of the conversation headerline. A fixed tag so the guard can
/// unregister it on teardown.
pub const HEADERLINE_PROVIDER_ID: ProviderId = 0x4155_0002;

/// The prompt marker rendered at the head of the editable tail line. Two bytes,
/// so [`EditableTail::first_line_min_byte`] is `2`: the marker itself is not
/// user-editable, only the text after it.
pub const PROMPT_MARKER: &str = "> ";

/// A token count for a headerline: `842`, `31.4K`, `1.2M`.
pub fn humanize(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// The mode id the host registers `Editor::run_editable_tail_invocation` under.
/// Any mode whose buffer is a transcript above an editable prompt returns it
/// from [`Mode::invocation_runner`](lattice_mode::Mode::invocation_runner) to
/// have vim operators gated to the prompt (a runner, unlike `read-only-mode`'s,
/// that lets an edit through when the cursor is in the tail).
pub fn editable_tail_runner() -> ModeId {
    ModeId::new("ai-conversation-mode")
}

/// The absolute line where the prompt begins for a given transcript: the
/// transcript's line count (each rendered turn ends in `\n`, so the trailing
/// `PROMPT_MARKER` lands on the next line).
pub fn prompt_anchor_line(transcript: &str) -> u32 {
    transcript.matches('\n').count() as u32
}

/// Per-buffer prompt anchors. Only a buffer's own activation inserts an entry
/// and only the guard of that activation removes it; the drain updates an entry
/// that is still its own, so a re-projection landing after deactivation cannot
/// resurrect a stale one.
///
/// Each seed is stamped with a generation. A buffer re-activated before its
/// previous guard drops gets a newer entry, and the old guard's removal (which
/// carries the old generation) leaves it alone.
#[derive(Clone, Default)]
pub struct PromptAnchors(Arc<AnchorState>);

#[derive(Default)]
struct AnchorState {
    entries: RwLock<HashMap<BufferId, (u64, u32)>>,
    next_generation: AtomicU64,
}

impl PromptAnchors {
    /// The anchor line for `buffer`, or `None` if it is not a seeded
    /// conversation (fully read-only, never another buffer's prompt).
    pub fn get(&self, buffer: BufferId) -> Option<u32> {
        let map = self.0.entries.read().unwrap_or_else(|e| e.into_inner());
        map.get(&buffer).map(|(_, line)| *line)
    }

    /// Install `line` as `buffer`'s anchor, replacing any previous activation's.
    /// Returns the generation the caller's guard must present to remove it.
    pub fn seed(&self, buffer: BufferId, line: u32) -> u64 {
        let generation = self.0.next_generation.fetch_add(1, Ordering::Relaxed) + 1;
        let mut map = self.0.entries.write().unwrap_or_else(|e| e.into_inner());
        map.insert(buffer, (generation, line));
        generation
    }

    /// Move `buffer`'s anchor, only if `generation` still owns it.
    pub fn update(&self, buffer: BufferId, generation: u64, line: u32) {
        let mut map = self.0.entries.write().unwrap_or_else(|e| e.into_inner());
        if let Some(slot) = map.get_mut(&buffer)
            && slot.0 == generation
        {
            slot.1 = line;
        }
    }

    /// Remove `buffer`'s anchor, only if `generation` still owns it. Returns
    /// whether it did, i.e. whether the caller is the buffer's current binding.
    pub fn remove_if(&self, buffer: BufferId, generation: u64) -> bool {
        let mut map = self.0.entries.write().unwrap_or_else(|e| e.into_inner());
        match map.get(&buffer) {
            Some((g, _)) if *g == generation => {
                map.remove(&buffer);
                true
            }
            _ => false,
        }
    }

    /// The editable tail for `buffer`: the single trailing prompt, editable
    /// after the marker, from the anchor line down.
    pub fn editable_tail(&self, buffer: BufferId) -> Option<EditableTail> {
        let anchor = self.get(buffer)?;
        Some(EditableTail {
            trailing_lines: 1,
            first_line_min_byte: PROMPT_MARKER.len() as u32,
            // Absolute anchor = the transcript-end line, kept current by the
            // drain. Lets the read-only gate cover a MULTI-line prompt (`<C-j>`):
            // every line at or below the anchor is editable, the transcript
            // above is frozen. `trailing_lines: 1` is the inert fallback for the
            // first frame before the anchor is seeded.
            first_editable_line: Some(anchor),
        })
    }

    // Read only by the ACP conversation mode's tests.
    #[cfg(all(test, feature = "acp"))]
    pub(crate) fn len(&self) -> usize {
        self.0
            .entries
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }
}

/// The minimal line-granular replacement turning `old` into `new`: the index of
/// the first differing line, and the replacement text for everything from that
/// line onward. `None` when the texts are already equal.
pub fn suffix_replace(old: &str, new: &str) -> Option<LineSuffixReplace> {
    if old == new {
        return None;
    }
    let old_lines: Vec<&str> = old.split('\n').collect();
    let new_lines: Vec<&str> = new.split('\n').collect();
    // Capped below the old line count: when the old text is a strict line-prefix
    // of the new (appended text opening with a newline), the last old line is
    // still the edit's end line, so the replacement must start there.
    let common = old_lines
        .iter()
        .zip(new_lines.iter())
        .take_while(|(a, b)| a == b)
        .count()
        .min(old_lines.len() - 1);
    Some(LineSuffixReplace {
        first_diff_line: common,
        old_line_count: old_lines.len(),
        replacement: new_lines[common..].join("\n"),
    })
}

/// Result of [`suffix_replace`]: replace lines `first_diff_line..old_line_count`
/// (0-based, the split-on-`\n` line grid) with `replacement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineSuffixReplace {
    pub first_diff_line: usize,
    pub old_line_count: usize,
    pub replacement: String,
}

/// (line, col) of the end of `s` on the split-on-`\n` line grid.
fn text_end(s: &str) -> (u32, u32) {
    let last_line = s.split('\n').count().saturating_sub(1);
    let last_len = s.rsplit('\n').next().map(str::len).unwrap_or(0);
    (last_line as u32, last_len as u32)
}

/// Replace the entire buffer with `text`.
pub async fn full_replace(handle: &Arc<dyn lattice_runtime::Document>, text: &str) {
    let snap = handle.snapshot();
    let last_line = snap.buffer.rope_line_count().saturating_sub(1); // CV.3: rope — whole-buffer extent
    let last_len = snap.buffer.line(last_line).unwrap_or_default().len() as u32;
    let range = lattice_protocol::Range::new(
        lattice_protocol::position::Position::new(0, 0),
        lattice_protocol::position::Position::new(last_line, last_len),
    );
    let edit = lattice_protocol::edit::Edit::replace(range, text.to_string());
    let _ = handle.apply_edit_batch(vec![edit]).await;
}

/// Re-project the transcript zone `last` → `new` into the buffer with one edit.
///
/// When `clear_prompt` (a prompt was just submitted) the edit extends through
/// the *current* prompt line to EOF and re-appends an empty `PROMPT_MARKER`, so
/// the transcript grows and the prompt resets in a single atomic edit — the
/// drain owns the prompt's lifecycle, so there is no separate clear edit to race
/// this re-projection. Otherwise the edit ends at the prompt-line start, leaving
/// the user's in-progress prompt intact while a reply streams.
/// Returns `true` when the buffer now matches `new` (applied, or already equal);
/// `false` when the edit failed, so the caller keeps `last` and retries.
async fn reproject(
    handle: &Arc<dyn lattice_runtime::Document>,
    last: &str,
    new: &str,
    clear_prompt: bool,
) -> bool {
    let Some(rep) = suffix_replace(last, new) else {
        return true; // transcript text unchanged
    };
    let (end, replacement) = if clear_prompt {
        let snap = handle.snapshot();
        let last_line = snap.buffer.rope_line_count().saturating_sub(1); // CV.3: rope — whole-buffer extent
        let last_len = snap.buffer.line(last_line).unwrap_or_default().len() as u32;
        (
            (last_line, last_len),
            format!("{}{}", rep.replacement, PROMPT_MARKER),
        )
    } else {
        (text_end(last), rep.replacement)
    };
    let range = lattice_protocol::Range::new(
        lattice_protocol::position::Position::new(rep.first_diff_line as u32, 0),
        lattice_protocol::position::Position::new(end.0, end.1),
    );
    let edit = lattice_protocol::edit::Edit::replace(range, replacement);
    match handle.apply_edit_batch(vec![edit]).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("transcript re-projection edit failed, will retry: {e:?}");
            false
        }
    }
}

/// Join the prompt lines (`anchor..=last`) into the text to send: strip the
/// `PROMPT_MARKER` from the first (anchor) line, take continuation lines (added
/// via `<C-j>`) verbatim, and join with `\n`. Pure so the multi-line read is
/// unit-testable without a live buffer.
pub fn assemble_prompt<I: IntoIterator<Item = String>>(lines: I) -> String {
    lines
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let line = line.trim_end_matches('\n');
            if i == 0 {
                line.strip_prefix(PROMPT_MARKER).unwrap_or(line).to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Read the prompt out of the action's buffer. The prompt spans `anchor..EOF`.
///
/// `None` when the buffer is not a seeded conversation: with no anchor there is
/// no prompt region, and guessing line 0 would send the whole transcript as if
/// the user had typed it. Also `None` for an empty prompt.
pub fn read_prompt(ctx: &ActionContext<'_>, anchors: &PromptAnchors) -> Option<String> {
    let store = ctx.services.get::<lattice_mode::BufferStoreHandle>()?;
    let buffer_id = BufferId(ctx.buffer_id.0 as u32);
    let handle = store.handle_for(buffer_id)?;
    let anchor = anchors.get(buffer_id)?;
    prompt_in(&handle.snapshot(), anchor)
}

/// The prompt in `snap`, a chat buffer whose prompt starts at line `anchor`;
/// `None` when it is empty. [`read_prompt`] for a caller holding the snapshot
/// and anchor rather than an action (`:llm-inspect`).
pub fn prompt_in(snap: &lattice_runtime::DocumentSnapshot, anchor: u32) -> Option<String> {
    // CV.3: content space — the prompt region is user-typed lines;
    // a phantom empty line after the terminating newline is not one.
    let last_line = snap.buffer.content_line_count().saturating_sub(1);
    let anchor_line = anchor.min(last_line);
    let prompt =
        assemble_prompt((anchor_line..=last_line).map(|l| snap.buffer.line(l).unwrap_or_default()));
    if prompt.trim().is_empty() {
        return None;
    }
    Some(prompt)
}

/// The body of the prompt's `<C-j>`: insert a literal newline at the cursor,
/// growing the multi-line prompt. `<CR>` is taken by send, so newline needs its
/// own chord. The cursor is always within the prompt (Insert only edits the
/// tail), so the insert stays inside the editable region.
pub fn newline_handler() -> ActionHandler {
    Arc::new(|ctx: &ActionContext<'_>| -> Option<Effect> {
        let target = BufferId(ctx.buffer_id.0 as u32);
        let edit = lattice_protocol::edit::Edit::insert(ctx.cursor, "\n".to_string());
        Some(Effect::ApplyEdit {
            target,
            edit,
            cursor: Some(lattice_protocol::position::Position::new(
                ctx.cursor.line + 1,
                0,
            )),
        })
    })
}

/// What [`bind`] needs to wire a buffer.
pub struct TranscriptBinding<'a> {
    pub ctx: &'a ModeContext,
    pub buffer_id: BufferId,
    pub handle: Arc<dyn lattice_runtime::Document>,
    /// The conversation this buffer projects.
    pub store: ConversationStore,
    pub anchors: PromptAnchors,
    /// The headerline row source, if the mode has one and a registrar exists.
    pub headerline: Option<Arc<dyn Headerline>>,
    /// Bumped by the drain on each re-projection so the headerline republishes.
    pub headerline_version: Arc<AtomicU64>,
    /// Re-project only for this session's updates. `None` follows every update
    /// (the single ACP conversation).
    pub only_session: Option<SessionKey>,
    pub runtime: tokio::runtime::Handle,
    /// How the transcript is projected. `ProjectionSource::default()` is the
    /// ACP display.
    pub projection: ProjectionSource,
    /// Text the prompt starts with, written in the same replace that seeds
    /// the buffer: a forked chat's question, ready to edit (LL.19).
    pub prompt: Option<String>,
}

/// The activation guard: the `ConversationUpdated` subscription plus this
/// buffer's registrations. Dropping it on deactivation unsubscribes, so a
/// stopped mode contributes no further work.
pub struct TranscriptGuard {
    _subscription: Subscription,
    anchors: PromptAnchors,
    buffer_id: BufferId,
    /// The generation of the anchor this activation seeded.
    generation: u64,
    headerline: Option<Arc<dyn VirtualRowRegistrar>>,
    fold_registrations: Vec<(
        lattice_core::FoldOverlayServiceHandle,
        lattice_core::ProviderId,
    )>,
}

impl Drop for TranscriptGuard {
    fn drop(&mut self) {
        // Only the activation that still owns the buffer tears down its
        // buffer-level registrations. A re-activation seeded a newer anchor and
        // registered its own headerline and fold sources, under the same ids
        // (the headerline's is fixed, the fold sources' are per buffer and
        // kind), and an old guard dropping late must not remove those.
        if !self.anchors.remove_if(self.buffer_id, self.generation) {
            return;
        }
        for (svc, id) in self.fold_registrations.drain(..) {
            svc.remove_source(id);
        }
        if let Some(registrar) = self.headerline.take() {
            registrar.unregister(self.buffer_id, HEADERLINE_PROVIDER_ID);
        }
    }
}

#[cfg(test)]
impl TranscriptGuard {
    /// A guard with no registrations, for tests of the anchor lifecycle.
    pub(crate) fn for_test(anchors: &PromptAnchors, buffer_id: BufferId, generation: u64) -> Self {
        let bus = Arc::new(lattice_runtime::EventBus::new());
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<ConversationUpdated>();
        let sub_id = bus.subscribe_typed::<ConversationUpdated>(tx);
        Self {
            _subscription: Subscription::new(bus, sub_id),
            anchors: anchors.clone(),
            buffer_id,
            generation,
            headerline: None,
            fold_registrations: Vec::new(),
        }
    }
}

/// Wire `b.buffer_id` to its conversation: seed the buffer and the anchor,
/// register the headerline and fold sources, and spawn the drain that
/// re-projects on every update.
pub async fn bind(b: TranscriptBinding<'_>) -> TranscriptGuard {
    let TranscriptBinding {
        ctx,
        buffer_id,
        handle,
        store,
        anchors,
        headerline,
        headerline_version,
        only_session,
        runtime,
        projection,
        prompt,
    } = b;

    // Sync the buffer to the current snapshot with a single full replace
    // (handles a fresh OR reopened buffer uniformly). `last` then tracks the
    // buffer content so subsequent updates diff self-consistently. It covers
    // the *conversation zone* only; the buffer is `{conversation}{marker}`
    // with the editable prompt as the trailing line.
    let seed = store.snapshot();
    // One options read and one projection feed BOTH the text and the folds: the
    // frame is what the fold sources read. Reading the options (or the store)
    // again from a source would let text and folds come from different moments.
    let frames = FrameCell::default();
    let seed_options = projection.get();
    let (seed_text, seed_folds) = project_conversation_with(&seed, &seed_options);
    frames.publish(ProjectionFrame {
        options: seed_options,
        folds: seed_folds,
    });
    let mut last = seed_text;
    let mut last_submissions = seed.submission_count();
    full_replace(
        &handle,
        &format!("{last}{PROMPT_MARKER}{}", prompt.unwrap_or_default()),
    )
    .await;

    let generation = anchors.seed(buffer_id, prompt_anchor_line(&last));

    let headerline_registrar = headerline.and_then(|row_source| {
        let registrar = ctx.service::<Arc<dyn VirtualRowRegistrar>>()?;
        let registrar: Arc<dyn VirtualRowRegistrar> = (*registrar).clone();
        let provider = Arc::new(HeaderlineProvider::new(HEADERLINE_PROVIDER_ID, row_source));
        // The provider id is a fixed tag, and `register` refuses to replace a
        // live id. Clear any registration a previous activation left behind so
        // a re-opened buffer binds its own headerline, not a stale one.
        registrar.unregister(buffer_id, HEADERLINE_PROVIDER_ID);
        registrar.register(buffer_id, provider as Arc<dyn VirtualRowProvider>);
        Some(registrar)
    });

    // Tool-call + reasoning folds. Each source holds a store clone and reads the
    // published snapshot on every `recompute_folds`.
    let fold_registrations = ctx
        .service::<lattice_core::FoldOverlayServiceHandle>()
        .map(|outer| {
            let svc = (*outer).clone();
            let tool: Arc<dyn lattice_core::FoldSource> = Arc::new(
                ToolCallFoldSource::new(store.clone(), buffer_id).with_frame(frames.clone()),
            );
            let tool_id = svc.add_source(tool, buffer_id);
            let reasoning: Arc<dyn lattice_core::FoldSource> = Arc::new(
                ReasoningFoldSource::new(store.clone(), buffer_id).with_frame(frames.clone()),
            );
            let reasoning_id = svc.add_source(reasoning, buffer_id);
            vec![(svc.clone(), tool_id), (svc, reasoning_id)]
        })
        .unwrap_or_default();

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ConversationUpdated>();
    let sub_id = ctx.events().subscribe_typed::<ConversationUpdated>(tx);
    let bus_handle = ctx.events_handle();
    // A separate handle for the drain to *publish* `ConversationProjected` once
    // its edit lands.
    let projected_bus = ctx.events_handle();

    let drain_anchors = anchors.clone();
    runtime.spawn(async move {
        // When the buffer was last written: the next write waits out the rest
        // of the frame, and what arrives meanwhile joins it (design §9: at
        // most one write per frame). The first write of a burst goes at once.
        let mut last_write: Option<tokio::time::Instant> = None;
        let wanted = |ev: &ConversationUpdated| {
            only_session
                .as_ref()
                .is_none_or(|session| *session == ev.session)
        };
        while let Some(first) = rx.recv().await {
            if let Some(at) = last_write {
                tokio::time::sleep_until(at + FRAME).await;
            }
            // Coalesce a burst of updates into one re-projection.
            let mut relevant = wanted(&first);
            while let Ok(ev) = rx.try_recv() {
                relevant |= wanted(&ev);
            }
            if !relevant {
                continue;
            }
            let snap: Conversation = store.snapshot();
            let options = projection.get();
            let (new, folds) = project_conversation_with(&snap, &options);
            // Published BEFORE the edit: the recompute the edit triggers then
            // reads folds for this very text. After it, that recompute could
            // run against the previous frame.
            frames.publish(ProjectionFrame { options, folds });
            let submissions = snap.submission_count();
            // Keep the editable-region anchor on the transcript-end line: a
            // re-projection may grow the transcript above the prompt (streaming)
            // or reset it (send), moving where the prompt begins. Store before
            // the edit so the gate is consistent with the new content on the
            // next keystroke.
            drain_anchors.update(buffer_id, generation, prompt_anchor_line(&new));
            // A new submission means the user just sent (or queued) a prompt.
            // Rewrite the tail through EOF and re-append an empty prompt marker,
            // clearing the prompt *atomically* with the transcript update. The
            // send handler deliberately emits no clear edit: a separate clear
            // would race this re-projection (which shifts the prompt line down)
            // and land on the wrong, now-read-only line. On other updates the
            // user's in-progress prompt is left untouched.
            let clear_prompt = submissions > last_submissions;
            let landed = reproject(&handle, &last, &new, clear_prompt).await;
            last_write = Some(tokio::time::Instant::now());
            headerline_version.fetch_add(1, Ordering::Release);

            // Wake the editor actor now that the edit has LANDED, so the update
            // repaints WITHOUT a keystroke. Published after the await — not via
            // `ConversationUpdated`, which fires before the buffer is
            // re-projected — so the wake never repaints stale content. Published
            // unconditionally, not gated on the text changing: the headerline
            // reads status, usage and queue length, none of which appear in the
            // transcript, and gating on text froze it at "Ready" once.
            projected_bus.publish_typed(ConversationProjected);
            if landed {
                last = new;
                last_submissions = submissions;
            }
        }
    });

    TranscriptGuard {
        _subscription: Subscription::new(bus_handle, sub_id),
        anchors,
        buffer_id,
        generation,
        headerline: headerline_registrar,
        fold_registrations,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn suffix_replace_handles_a_strict_line_prefix() {
        // New text appended after the old trailing blank line, opening with a newline.
        let r = suffix_replace("a\n\n", "a\n\n\nb\n\n").unwrap();
        assert_eq!(r.old_line_count, 3);
        assert!(r.first_diff_line < r.old_line_count, "{r:?}");
        assert_eq!(r.first_diff_line, 2);
        assert_eq!(r.replacement, "\nb\n\n");
        let old: Vec<&str> = "a\n\n".split('\n').collect();
        let rebuilt = [&old[..r.first_diff_line], &[r.replacement.as_str()]]
            .concat()
            .join("\n");
        assert_eq!(rebuilt, "a\n\n\nb\n\n");
        assert_eq!(suffix_replace("x", "x"), None);
    }

    #[test]
    fn prompt_anchor_line_is_transcript_line_count() {
        assert_eq!(prompt_anchor_line(""), 0);
        assert_eq!(prompt_anchor_line("you:\nhi\n"), 2);
        assert_eq!(prompt_anchor_line("a\nb\nc\n"), 3);
    }

    #[test]
    fn assemble_prompt_reads_multiline_prompt() {
        assert_eq!(assemble_prompt(["> hello".to_string()]), "hello");
        assert_eq!(
            assemble_prompt([
                "> line1".to_string(),
                "line2".to_string(),
                "line3".to_string()
            ]),
            "line1\nline2\nline3",
        );
        assert_eq!(
            assemble_prompt(["> a\n".to_string(), "b\n".to_string()]),
            "a\nb"
        );
        assert_eq!(assemble_prompt(["plain".to_string()]), "plain");
    }

    /// A re-activation seeds a newer anchor before the old guard drops. The old
    /// guard's teardown must not take the new activation's anchor with it.
    #[test]
    fn a_late_old_guard_does_not_remove_the_new_anchor() {
        let anchors = PromptAnchors::default();
        let b = BufferId(1);
        let old_gen = anchors.seed(b, 3);
        let old_guard = TranscriptGuard::for_test(&anchors, b, old_gen);
        let new_gen = anchors.seed(b, 9);
        let _new_guard = TranscriptGuard::for_test(&anchors, b, new_gen);

        drop(old_guard);
        assert_eq!(
            anchors.get(b),
            Some(9),
            "the new activation keeps its anchor"
        );
        assert_eq!(
            anchors.editable_tail(b).unwrap().first_editable_line,
            Some(9)
        );
    }

    /// The fold sources are registered under per-buffer ids, so a
    /// re-activation's replace the old ones; the old guard dropping late
    /// must not remove them. The owner's drop does.
    #[test]
    fn a_late_old_guard_leaves_the_new_fold_sources() {
        #[derive(Default)]
        struct Folds(std::sync::Mutex<Vec<lattice_core::ProviderId>>);
        impl lattice_core::FoldOverlayService for Folds {
            fn add_source(
                &self,
                _: Arc<dyn lattice_core::FoldSource>,
                _: BufferId,
            ) -> lattice_core::ProviderId {
                lattice_core::ProviderId(0)
            }
            fn remove_source(&self, id: lattice_core::ProviderId) {
                self.0.lock().unwrap().push(id);
            }
        }
        let folds = Arc::new(Folds::default());
        let anchors = PromptAnchors::default();
        let b = BufferId(1);
        let id = lattice_core::ProviderId(super::super::tool_fold::TOOL_FOLD_NAMESPACE | 1);
        let guard = |generation| {
            let mut g = TranscriptGuard::for_test(&anchors, b, generation);
            g.fold_registrations
                .push((folds.clone() as lattice_core::FoldOverlayServiceHandle, id));
            g
        };
        let old = guard(anchors.seed(b, 3));
        let new = guard(anchors.seed(b, 9));
        drop(old);
        assert!(folds.0.lock().unwrap().is_empty(), "the new sources stay");
        drop(new);
        assert_eq!(*folds.0.lock().unwrap(), [id], "the owner removes them");
    }

    /// The old activation's drain, still running, must not move the new anchor.
    #[test]
    fn a_stale_drain_does_not_move_the_new_anchor() {
        let anchors = PromptAnchors::default();
        let b = BufferId(1);
        let old_gen = anchors.seed(b, 3);
        let new_gen = anchors.seed(b, 9);
        anchors.update(b, old_gen, 99);
        assert_eq!(anchors.get(b), Some(9));
        anchors.update(b, new_gen, 12);
        assert_eq!(anchors.get(b), Some(12));
    }

    #[test]
    fn removing_the_current_generation_clears_the_anchor() {
        let anchors = PromptAnchors::default();
        let b = BufferId(1);
        let g = anchors.seed(b, 3);
        assert!(anchors.remove_if(b, g));
        assert_eq!(anchors.get(b), None);
        assert!(!anchors.remove_if(b, g), "already gone");
    }
}
