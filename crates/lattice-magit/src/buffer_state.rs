//! MG.13 — per-buffer mode state, published for boot-registered
//! action handlers.
//!
//! **The problem this closes.** magit's per-buffer modes used to
//! register their action handlers from inside `on_activate`, closing
//! over an `Arc<Mutex<…State>>` built during activation. `on_activate`
//! runs in the cascade future that `ModeRegistry::spawn_cascade`
//! spawns, so there is a window after a magit buffer opens in which
//! the chord resolves through the keymap, the mode reads as active,
//! and **no handler exists** — the keypress does nothing. That is not
//! a test artefact: it is what a user gets pressing `d` quickly after
//! `:magit-branch`. It is also the exact bug MG.8 shipped
//! (`MagitGlobalMode` registered from `on_activate` behind a
//! `OnceLock`); the fix there was to move to `Mode::action_handlers()`,
//! and this module finishes that migration for the modes that were
//! left behind because they carry per-buffer state.
//!
//! **The shape.** Handlers move to `Mode::action_handlers()` —
//! registered once at boot, for the lifetime of the app — and read
//! their per-buffer state out of a [`BufferStates`] service keyed by
//! `BufferId` at call time. `ActionContext` already carries both
//! `buffer_id` and `services`, so the handler has everything it needs.
//! Chord scoping is unchanged: K.1.c's per-keystroke filter only
//! routes a mode's chords in buffers where that mode is active.
//!
//! **Why the state is there in time.** `spawn_cascade` polls the
//! cascade future **once, synchronously, on the App thread** before
//! spawning it (its try-sync-then-spawn arm). Everything in
//! `on_activate` above its first `.await` therefore runs before
//! `activate_major` returns. Publishing state there makes it visible
//! to the very next keystroke — so each mode's `on_activate` must
//! `publish` before it awaits anything. Fields that genuinely cannot
//! be known until after an await (rebase's resolved `upstream`,
//! commit's `diff_end_line`) are published with an inert initial value
//! and filled in through the `Arc<Mutex<_>>` once known; their
//! handlers already refuse to act on the inert value.
//!
//! **Caveat — cascade position.** The synchronous first poll reaches
//! only as far as the first pending await in the *whole* cascade, so
//! only the root step (the major mode) is guaranteed to publish
//! synchronously. Implied minors run later. `magit-core-mode` is a
//! minor and must therefore not depend on this guarantee — which it
//! does not: its handlers read the buffer through `BufferStoreHandle`
//! and `ctx.buffer_id`, so they need no published state at all.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice_core::BufferId;
use lattice_mode::ActionContext;

/// Per-buffer state for one magit mode.
///
/// One instance per state type, registered as a service at install
/// time. Entries are published by `on_activate` and removed when the
/// buffer's mode guard drops, so a stale entry cannot outlive its
/// buffer.
pub struct BufferStates<S> {
    map: Mutex<HashMap<BufferId, Arc<Mutex<S>>>>,
}

impl<S> Default for BufferStates<S> {
    fn default() -> Self {
        Self {
            map: Mutex::new(HashMap::new()),
        }
    }
}

impl<S> BufferStates<S> {
    /// Publish `state` for `buffer`, replacing any prior entry (a
    /// re-activation on the same buffer supersedes the old state).
    /// Returns the shared handle so the caller can keep mutating it
    /// after an await — see the module note on late-resolved fields.
    pub fn publish(&self, buffer: BufferId, state: S) -> Arc<Mutex<S>> {
        let shared = Arc::new(Mutex::new(state));
        if let Ok(mut map) = self.map.lock() {
            map.insert(buffer, shared.clone());
        }
        shared
    }

    /// Publish an already-shared state handle.
    ///
    /// The peer of [`Self::publish`] for a mode that already owns an
    /// `Arc<Mutex<S>>` (magit-status builds one for its fold source
    /// before publishing).
    pub fn publish_shared(&self, buffer: BufferId, state: Arc<Mutex<S>>) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(buffer, state);
        }
    }

    /// The state for `buffer`, or `None` when no magit mode of this
    /// type is live on it. Handlers treat `None` as "not mine" and
    /// no-op — the same outcome as before, minus the race.
    pub fn get(&self, buffer: BufferId) -> Option<Arc<Mutex<S>>> {
        self.map.lock().ok()?.get(&buffer).cloned()
    }

    pub fn remove(&self, buffer: BufferId) {
        if let Ok(mut map) = self.map.lock() {
            map.remove(&buffer);
        }
    }

    /// Every live state of this type, in no particular order.
    ///
    /// MG.21c: for handlers that must reach their mode's buffers
    /// *without* being able to name one. A prompt's `-finish` action
    /// fires with the PROMPT buffer's `buffer_id` (see
    /// `Editor::do_prompt_line_submit`), so [`Self::get`] and
    /// [`state_for`] both return `None` there — but the work it just
    /// did still has to show up. Services are reachable from any
    /// context, so the finish handler refreshes through this instead.
    pub fn all(&self) -> Vec<Arc<Mutex<S>>> {
        match self.map.lock() {
            Ok(map) => map.values().cloned().collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Look up this mode's state for the buffer the action fired in.
///
/// `Handle` must be the same type used to register the service —
/// `ServiceRegistry` keys on `TypeId`, so a mismatch silently returns
/// `None` (see `feedback_servicesregistry_arc_typeid`). Each mode
/// defines exactly one `…StatesHandle` alias and uses it for both.
pub fn state_for<S: Send + Sync + 'static>(ctx: &ActionContext<'_>) -> Option<Arc<Mutex<S>>> {
    let states = ctx.services.get::<Arc<BufferStates<S>>>()?;
    states.get(BufferId(ctx.buffer_id.0 as u32))
}

/// Which tree a stretch of diff text was produced against — the
/// answer [`MagitView::diff_source`] gives, and the only thing
/// hunk-level `s` / `u` / `x` need to know beyond the hunk itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSource {
    /// `git diff --cached` — HEAD vs the index. `u` reverses it out.
    Staged,
    /// `git diff` — the index vs the working tree. `s` applies it in,
    /// `x` reverses it out of the worktree.
    Unstaged,
    /// MG.23g: a patch already in history — a commit's `git show`, a
    /// stash's `git stash show -p`.
    ///
    /// Neither `s` nor `u` can act on it: the change is not sitting
    /// between two of *this* checkout's trees, it is a description of
    /// something that already happened. What it supports instead is
    /// `a` — apply this one hunk to the working tree — and `-` —
    /// reverse it back out. Cherry-picking or reverting one hunk of a
    /// commit rather than the whole thing.
    Committed,
}

/// A magit buffer's view behaviour, published per buffer alongside
/// its state.
///
/// **Why this exists.** Several magit modes bind the *same* action —
/// `action:magit-refresh` (`gr`) has five registrants (status, branch,
/// stash, diff, log). Per-activation registration hid the collision:
/// only the active buffer's mode had a handler installed at any
/// moment. Boot-time registration does not, and
/// `ActionHandlerRegistry::register` *inserts* — last writer wins — so
/// five boot registrations would leave `gr` working in exactly one
/// view and silently dead in the other four.
///
/// The fix is polymorphism rather than a central `match`: **one**
/// handler for the shared action, owned by the mode that owns the
/// chord (`magit-core-mode` owns `gr`), dispatching through this trait
/// to whichever view is published for the buffer. Each view mode still
/// owns its own refresh body — which is what mode-ownership requires —
/// and no code branches on buffer kind.
///
/// **Do not mix registration styles for one action id.** Dropping an
/// `ActionHandlerRegistration` unregisters *by action id*, so a mode
/// that still registers `action:magit-refresh` from `on_activate` will,
/// on deactivation, remove the boot-registered handler too and break
/// `gr` everywhere. Any action reachable from more than one mode must
/// be boot-registered exactly once and dispatched through here.
pub trait MagitView: Send + Sync + 'static {
    /// `gr` — rebuild this view's content in place.
    fn refresh(&self) -> Option<lattice_grammar::Effect>;

    /// MG.18d: rebuild after a mutation, then put the cursor back on
    /// the work `restore` describes.
    ///
    /// Separate from [`Self::refresh`] because only a *mutation* has
    /// something to restore to: a bare `gr` leaves the cursor where the
    /// user parked it. The view resolves it because only the view knows
    /// its buffer's shape — magit-status looks for an entry row, a diff
    /// buffer for a `diff --git` header.
    ///
    /// The default is a plain refresh: a view that cannot say where the
    /// work went rebuilds and leaves the cursor alone, which is the
    /// pre-MG.18d behaviour.
    fn refresh_restoring(
        &self,
        site: crate::cursor_restore::HunkSite,
    ) -> Option<lattice_grammar::Effect> {
        let _ = site;
        self.refresh()
    }

    /// `s` — stage the entry at `cursor`.
    ///
    /// Bound by `magit-status-mode` and `magit-diff-mode`, which read
    /// their own buffer's format to find the path (a status entry line
    /// vs. the nearest `diff --git` header). Views that offer no
    /// staging decline, which is what the default does — `magit-log`
    /// has no `s` chord, so its view is never asked.
    fn stage(
        &self,
        cursor: lattice_protocol::position::Position,
    ) -> Option<lattice_grammar::Effect> {
        let _ = cursor;
        None
    }

    /// `u` — unstage the entry at `cursor`. Peer of [`Self::stage`].
    fn unstage(
        &self,
        cursor: lattice_protocol::position::Position,
    ) -> Option<lattice_grammar::Effect> {
        let _ = cursor;
        None
    }

    /// MG.18c: which diff the text at `cursor` came from.
    ///
    /// **Why staging has to ask.** A hunk's patch is only meaningful
    /// against the tree it was diffed from: an unstaged hunk applies
    /// forward into the index (`s`), a staged one reverses back out of
    /// it (`u`). Pressing the wrong one produces a patch git refuses,
    /// which reaches the user as `error: patch does not apply` —
    /// indistinguishable from a missed keypress. Worse, `x` on a
    /// staged hunk would reverse it out of the *worktree* while
    /// leaving it in the index: the change vanishes from the file but
    /// is still committed by the next `cc`.
    ///
    /// So the operation asks first and declines with a sentence.
    /// Every view answers from what it already knows — magit-status
    /// from the section header above the cursor, magit-diff from the
    /// scope in its buffer name.
    ///
    /// `None` means "not classifiable here", and hunk-level staging is
    /// refused rather than guessed: `*magit:diff*` (against HEAD)
    /// mixes both sides in one hunk, and a commit's or stash's inline
    /// patch in magit-status belongs to neither tree. File-level
    /// staging is unaffected — it never needed this answer.
    fn diff_source(&self, cursor: lattice_protocol::position::Position) -> Option<DiffSource> {
        let _ = cursor;
        None
    }

    /// MG.20: the commit this view describes at `cursor`, if any.
    ///
    /// Reset, revert and cherry-pick all mean "act on the commit under
    /// the cursor", and every view that shows commits answers that
    /// question differently — a log row, a `--stat` header, a rebase
    /// todo line, a Recent-commits entry. Rather than a handler per
    /// view (which the shared-action collision in MG.13 showed does not
    /// work) or a `match buffer_kind` in the host (which the
    /// everything-is-a-buffer rule forbids), each view answers here and
    /// `magit-core-mode` owns one handler per operation.
    ///
    /// Views with no commits decline, which is what the default does.
    fn commit_at_cursor(&self, cursor: lattice_protocol::position::Position) -> Option<String> {
        let _ = cursor;
        None
    }

    /// MG.22: **which version** of `path` `<CR>` should open, for a
    /// cursor sitting in this view's diff content.
    ///
    /// The split is the point. *Finding* the path is diff-text parsing
    /// and identical everywhere, so it belongs to `magit-hunk-mode`
    /// ([`crate::hunk::path_at_cursor`]) — three modes had a copy, and
    /// one of them had a bug the other two did not. *Choosing the
    /// version* is genuinely per-view and cannot be shared:
    ///
    /// | View | `<CR>` opens |
    /// |---|---|
    /// | magit-diff, staged scope | the index blob |
    /// | magit-diff, unstaged / HEAD scope | the working-tree file |
    /// | magit-commit | the index blob (its diff IS the index) |
    /// | magit-revision | the file at that sha |
    /// | magit-stash-show | the file as the stash left it |
    ///
    /// `None` means "this view has no answer for that path", and the
    /// caller says so rather than guessing at a version — opening the
    /// working-tree copy when the user asked for a historical one is
    /// the mistake `magit-file-revision-mode` exists to prevent.
    fn diff_target(&self, path: &std::path::Path) -> Option<lattice_grammar::Effect> {
        let _ = path;
        None
    }

    /// MG.22: what `<CR>` does when the cursor is **not** in diff
    /// content.
    ///
    /// Only magit-status needs this, and it is why `<CR>` could not
    /// simply move to `magit-hunk-mode` wholesale: there the chord is
    /// context-aware over rows that are not diffs at all — a file
    /// entry, a stash, a commit — and a minor's binding wins over a
    /// major's, so taking the chord without carrying that behaviour
    /// would have silently replaced it with a diff-only handler.
    ///
    /// Views whose buffer is entirely diff content never reach this.
    fn visit_at_cursor(
        &self,
        cursor: lattice_protocol::position::Position,
    ) -> Option<lattice_grammar::Effect> {
        let _ = cursor;
        None
    }

    /// The workdir this view's repository lives in — needed to run an
    /// operation against it from a handler that holds only the view.
    fn workdir(&self) -> Option<std::path::PathBuf> {
        None
    }
}

/// Per-buffer [`MagitView`] registry — the shared-action peer of
/// [`BufferStates`].
#[derive(Default)]
pub struct MagitViews {
    map: Mutex<HashMap<BufferId, Arc<dyn MagitView>>>,
}

/// Service alias — register and look up through this exact type
/// (`feedback_servicesregistry_arc_typeid`).
pub type MagitViewsHandle = Arc<MagitViews>;

impl MagitViews {
    pub fn publish(&self, buffer: BufferId, view: Arc<dyn MagitView>) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(buffer, view);
        }
    }

    pub fn get(&self, buffer: BufferId) -> Option<Arc<dyn MagitView>> {
        self.map.lock().ok()?.get(&buffer).cloned()
    }

    pub fn remove(&self, buffer: BufferId) {
        if let Ok(mut map) = self.map.lock() {
            map.remove(&buffer);
        }
    }
}

/// The view for the buffer an action fired in.
pub fn view_for(ctx: &ActionContext<'_>) -> Option<Arc<dyn MagitView>> {
    let views = ctx.services.get::<MagitViewsHandle>()?;
    views.get(BufferId(ctx.buffer_id.0 as u32))
}

/// Wraps a mode's existing Guard and additionally unpublishes its
/// [`MagitView`] on drop.
///
/// Retained as the composition point for a mode whose Guard already
/// carries other teardown (magit-status's fold-source registration)
/// and which also publishes a view.
pub struct ViewGuard<G> {
    _inner: G,
    views: MagitViewsHandle,
    buffer: BufferId,
}

impl<G> ViewGuard<G> {
    pub fn new(inner: G, views: MagitViewsHandle, buffer: BufferId) -> Self {
        Self {
            _inner: inner,
            views,
            buffer,
        }
    }
}

impl<G> Drop for ViewGuard<G> {
    fn drop(&mut self) {
        self.views.remove(self.buffer);
    }
}

/// Drops a buffer's state entry when its mode deactivates.
///
/// Handler registrations are no longer per-activation anywhere in this
/// crate, so what is left to unwind is the state entry, the published
/// view, and (MG.14) the headerline's virtual-row provider.
pub struct BufferStateGuard<S: Send + Sync + 'static> {
    states: Arc<BufferStates<S>>,
    /// Set when the mode also published a [`MagitView`]; dropped
    /// together with the state so a dead buffer's `gr` cannot resolve.
    views: Option<MagitViewsHandle>,
    /// MG.14: the headerline provider registration. Its own `Drop`
    /// unregisters — holding it here just ties its lifetime to the
    /// mode's, so the sticky row disappears with the mode.
    _headerline: Option<crate::headerline::HeaderlineRegistration>,
    buffer: BufferId,
}

impl<S: Send + Sync + 'static> BufferStateGuard<S> {
    pub fn new(states: Arc<BufferStates<S>>, buffer: BufferId) -> Self {
        Self {
            states,
            views: None,
            _headerline: None,
            buffer,
        }
    }

    /// Also unpublish this buffer's [`MagitView`] on drop.
    pub fn with_views(mut self, views: MagitViewsHandle) -> Self {
        self.views = Some(views);
        self
    }

    /// Also tear down this buffer's headerline on drop. `None` (a
    /// harness with no virtual-row registrar) is a no-op.
    pub fn with_headerline(
        mut self,
        registration: Option<crate::headerline::HeaderlineRegistration>,
    ) -> Self {
        self._headerline = registration;
        self
    }
}

impl<S: Send + Sync + 'static> Drop for BufferStateGuard<S> {
    fn drop(&mut self) {
        self.states.remove(self.buffer);
        if let Some(views) = &self.views {
            views.remove(self.buffer);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq)]
    struct Probe(u32);

    #[test]
    fn published_state_is_readable_for_its_buffer_only() {
        let states: BufferStates<Probe> = BufferStates::default();
        states.publish(BufferId(1), Probe(7));
        assert_eq!(states.get(BufferId(1)).unwrap().lock().unwrap().0, 7);
        assert!(
            states.get(BufferId(2)).is_none(),
            "a handler firing in another buffer must not see this state"
        );
    }

    /// The late-resolved-field path: `on_activate` publishes before it
    /// awaits, then fills in what it learns afterwards. A handler that
    /// ran in between sees the inert initial value, never a missing
    /// entry.
    #[test]
    fn state_published_before_an_await_is_mutable_afterwards() {
        let states: BufferStates<Probe> = BufferStates::default();
        let shared = states.publish(BufferId(1), Probe(0));
        shared.lock().unwrap().0 = 42;
        assert_eq!(states.get(BufferId(1)).unwrap().lock().unwrap().0, 42);
    }

    /// Re-activating on the same buffer must supersede, not
    /// accumulate — otherwise a reopened magit buffer's chords would
    /// act on the previous session's state.
    #[test]
    fn republishing_supersedes_the_previous_entry() {
        let states: BufferStates<Probe> = BufferStates::default();
        states.publish(BufferId(1), Probe(1));
        states.publish(BufferId(1), Probe(2));
        assert_eq!(states.get(BufferId(1)).unwrap().lock().unwrap().0, 2);
    }

    #[test]
    fn guard_drop_removes_the_entry_so_it_cannot_outlive_the_buffer() {
        let states: Arc<BufferStates<Probe>> = Arc::new(BufferStates::default());
        states.publish(BufferId(1), Probe(1));
        let guard = BufferStateGuard::new(states.clone(), BufferId(1));
        drop(guard);
        assert!(states.get(BufferId(1)).is_none());
    }
}
