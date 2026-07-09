//! IDE-protocol I7: the `claude-code` modeline status segment.
//!
//! Shows the IDE server's state — running/port + live connection count — on the
//! modeline of every buffer running `claude-code-mode` (the agent terminal).
//! Mode-owned per `feedback_mode_owns_its_surface`: `claude-code-mode`'s
//! `on_activate` registers its buffer here and the returned Guard unregisters it
//! on deactivate; the descriptor + the off-thread publisher live in this crate
//! (the host only owns the generic `ModelineService` + the render path).
//!
//! Mirrors `lattice-lsp::modeline`: a producer pushes content keyed
//! `ModelineKey::Buffer(id)` over the event bus (ML.3); the host's §12 wake
//! forwarder turns each push into an off-keystroke repaint, so nothing runs on
//! the render path (paramount #1). The publisher only republishes when the
//! rendered text actually changes, so a quiescent server produces no repaints.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use lattice_core::BufferId;
use lattice_mode::{
    modeline::ROLE_MODE_ITEM, ElementContent, ElementId, ModelineElement, ModelineElementUpdate,
    ModelineKey, ModelineRole, ModelineService, Zone,
};
use lattice_runtime::EventBus;
use tokio::sync::Notify;

use crate::mcp::server::ServerState;

/// Modeline element id — owned by the MCP adapter (`lattice_ai::mcp`)
/// (`feedback_mode_owns_its_surface`; the namespace is the owner key).
pub const STATUS_ELEMENT: &str = "claude-code";

/// The set of buffers (agent terminals) currently showing the status, shared
/// between the mode (registers/unregisters) and the publisher task (reads).
pub type IdeBuffers = Arc<Mutex<HashSet<BufferId>>>;

/// Register the `claude-code` descriptor with the host's modeline registry.
/// `Right` zone, low priority so it sits near the LSP / position elements.
pub fn register_status_descriptor(svc: &ModelineService) {
    svc.register(ModelineElement::new(
        ElementId::new(STATUS_ELEMENT),
        Zone::Right,
        6,
    ));
}

/// Build the status content for the current server state. Empty when the server
/// is stopped (the element hides itself — no stale "off" badge cluttering the
/// modeline when the IDE isn't running).
///
/// Format: `<glyph> claude · <project> :<port> · <conns>`, e.g.
/// `● claude · lattice :8123 · 1 conn` (an agent attached) or
/// `○ claude · lattice :8123` (server up, waiting for an agent). The leading
/// glyph is a universal BMP shape (`●` connected / `○` waiting — same cell
/// width, renders in every monospace font, per the icon-palette rule). The
/// project segment is omitted when no workspace folder is known.
pub fn status_content(
    state: &ServerState,
    conns: usize,
    project: &str,
    reviews: usize,
    mention: bool,
) -> ElementContent {
    if !state.running {
        return ElementContent::default();
    }
    // ● = an agent is attached; ○ = running but no agent yet.
    let glyph = if conns > 0 { '●' } else { '○' };
    let port = state.port.map(|p| p.to_string()).unwrap_or_default();
    let proj = if project.is_empty() {
        String::new()
    } else {
        format!(" · {project}")
    };
    let conn = match conns {
        0 => String::new(),
        1 => " · 1 conn".to_string(),
        n => format!(" · {n} conns"),
    };
    // `◆ review` when an openDiff is awaiting your accept/reject — the agent
    // is blocked on you. `◆` is a BMP fallback-palette shape (universal).
    let review = match reviews {
        0 => String::new(),
        1 => " · ◆ review".to_string(),
        n => format!(" · ◆ {n} reviews"),
    };
    // `@sent` briefly after `:claude-send` pushes context to the agent.
    let mention = if mention { " · @sent" } else { "" };
    let text = format!("{glyph} claude{proj} :{port}{conn}{review}{mention}");
    ElementContent::text(text, ModelineRole::new(ROLE_MODE_ITEM))
}

/// D-fix.6 follow-up: a transient `@sent` echo on the modeline after a
/// `:claude-send` pushes context to the agent. Unlike the review badge (a
/// lifecycle span), an at-mention is a momentary event, so the echo is
/// time-boxed: `ping()` stamps a clear-deadline + wakes the publisher, which
/// shows `@sent` until the deadline then clears itself (the publisher waits on
/// the wake OR the deadline, whichever comes first).
pub struct MentionState {
    until: Mutex<Option<std::time::Instant>>,
    changed: Arc<Notify>,
}

/// Cheap-clone handle to the shared [`MentionState`].
pub type MentionHandle = Arc<MentionState>;

/// How long the `@sent` echo stays on the modeline after a `:claude-send`.
const MENTION_ECHO: std::time::Duration = std::time::Duration::from_secs(3);

impl MentionState {
    pub fn new(changed: Arc<Notify>) -> MentionHandle {
        Arc::new(Self {
            until: Mutex::new(None),
            changed,
        })
    }

    /// Show `@sent` for [`MENTION_ECHO`] from now + wake the publisher.
    pub fn ping(&self) {
        *self.until.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(std::time::Instant::now() + MENTION_ECHO);
        self.changed.notify_one();
    }

    /// Time left before the echo clears, or `None` when it's inactive/expired.
    /// The publisher both renders on this (`Some` ⇒ show `@sent`) and sleeps on
    /// it to clear the echo when no other wake arrives first.
    pub fn remaining(&self) -> Option<std::time::Duration> {
        let until = *self.until.lock().unwrap_or_else(|e| e.into_inner());
        until.and_then(|d| d.checked_duration_since(std::time::Instant::now()))
    }
}

/// D-fix.6 follow-up: shared pending-review counter, mode-owned. claude-code
/// *produces* every openDiff and *awaits* its outcome (`diff::open_diff`), so
/// the span from request-sent to outcome-resolved IS a pending review — no host
/// signal needed. `begin()` (called by `open_diff` around its `await`) bumps
/// the count + wakes the status publisher; the returned [`ReviewGuard`]
/// decrements + wakes on Drop, so the count is correct even if the connection
/// drops or the task is cancelled mid-review.
pub struct ReviewState {
    count: Arc<AtomicUsize>,
    changed: Arc<Notify>,
}

/// Cheap-clone handle to the shared [`ReviewState`].
pub type ReviewHandle = Arc<ReviewState>;

impl ReviewState {
    /// Build a tracker whose mutations wake `changed` (the status publisher's
    /// wake), so a pending-review badge appears/clears off-keystroke.
    pub fn new(changed: Arc<Notify>) -> ReviewHandle {
        Arc::new(Self {
            count: Arc::new(AtomicUsize::new(0)),
            changed,
        })
    }

    /// The shared count cell the status publisher reads each wake.
    pub fn count_handle(&self) -> Arc<AtomicUsize> {
        self.count.clone()
    }

    /// Mark one review pending until the returned guard drops.
    pub fn begin(&self) -> ReviewGuard {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.changed.notify_one();
        ReviewGuard {
            count: self.count.clone(),
            changed: self.changed.clone(),
        }
    }
}

/// RAII guard: decrements the pending-review count + wakes the publisher on
/// Drop. Held across `open_diff`'s `await` so a cancelled / dropped review
/// can't leak the count high.
pub struct ReviewGuard {
    count: Arc<AtomicUsize>,
    changed: Arc<Notify>,
}

impl Drop for ReviewGuard {
    fn drop(&mut self) {
        self.count.fetch_sub(1, Ordering::Relaxed);
        self.changed.notify_one();
    }
}

/// The project name shown in the status segment — the basename of the first
/// workspace folder (e.g. `/home/me/src/lattice` → `lattice`). Empty when no
/// workspace folder is configured, in which case the project segment is hidden.
pub fn project_name(workspace_folders: &[String]) -> String {
    workspace_folders
        .first()
        .map(|f| f.trim_end_matches('/'))
        .filter(|f| !f.is_empty())
        .and_then(|f| std::path::Path::new(f).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Spawn the status publisher task. Wakes on `changed` (start/stop, a
/// connection open/close, or a buffer (un)registering), rebuilds the content,
/// and republishes it to each registered buffer **only when it differs** from
/// what that buffer last showed; a removed buffer is cleared (empty content).
pub fn spawn_status_publisher(
    bus: Arc<EventBus>,
    state: Arc<ArcSwap<ServerState>>,
    conn_count: Arc<AtomicUsize>,
    // D-fix follow-up: the project the agent runs for (static per server —
    // the workspace basename), shown in the segment. Empty ⇒ omitted.
    project: String,
    // D-fix.6 follow-up: the shared pending-review count (from `ReviewState`);
    // drives the `◆ review` badge.
    review_count: Arc<AtomicUsize>,
    // D-fix.6 follow-up: the transient `@sent` echo (from `MentionState`).
    mention: MentionHandle,
    ide_buffers: IdeBuffers,
    changed: Arc<Notify>,
    rt: &tokio::runtime::Handle,
) {
    rt.spawn(async move {
        let id = ElementId::new(STATUS_ELEMENT);
        let mut last: HashMap<BufferId, ElementContent> = HashMap::new();
        loop {
            // `@sent` is shown while the echo is unexpired; `mention_clear` is
            // how long until it clears (so we can wake ourselves to redraw).
            let mention_clear = mention.remaining();
            let content = status_content(
                &state.load(),
                conn_count.load(Ordering::Relaxed),
                &project,
                review_count.load(Ordering::Relaxed),
                mention_clear.is_some(),
            );
            let bufs: Vec<BufferId> = {
                let g = ide_buffers.lock().unwrap_or_else(|e| e.into_inner());
                g.iter().copied().collect()
            };

            // Publish for registered buffers whose content changed.
            for buf in &bufs {
                if last.get(buf) != Some(&content) {
                    publish(&bus, &id, *buf, content.clone());
                    last.insert(*buf, content.clone());
                }
            }
            // Clear buffers that unregistered (publish empty → element hides).
            let removed: Vec<BufferId> =
                last.keys().filter(|b| !bufs.contains(b)).copied().collect();
            for buf in removed {
                publish(&bus, &id, buf, ElementContent::default());
                last.remove(&buf);
            }

            // Wait for the next change. When the `@sent` echo is showing, also
            // wake at its clear deadline so it disappears on its own if nothing
            // else changes first.
            match mention_clear {
                Some(rem) => {
                    tokio::select! {
                        _ = changed.notified() => {}
                        _ = tokio::time::sleep(rem) => {}
                    }
                }
                None => changed.notified().await,
            }
        }
    });
}

fn publish(bus: &EventBus, id: &ElementId, buf: BufferId, content: ElementContent) {
    bus.publish_typed(ModelineElementUpdate {
        key: ModelineKey::Buffer(buf),
        id: id.clone(),
        content,
    });
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn state(running: bool, port: Option<u16>) -> ServerState {
        ServerState { running, port }
    }

    #[test]
    fn stopped_server_has_empty_content() {
        assert!(status_content(&state(false, None), 0, "lattice", 0, false).is_empty());
    }

    #[test]
    fn running_server_shows_glyph_project_port_and_conn_count() {
        // ○ = waiting (no agent), ● = connected. Project after `claude`.
        let c = status_content(&state(true, Some(8123)), 0, "lattice", 0, false);
        assert_eq!(c.plain(), "○ claude · lattice :8123");
        let c1 = status_content(&state(true, Some(8123)), 1, "lattice", 0, false);
        assert_eq!(c1.plain(), "● claude · lattice :8123 · 1 conn");
        let c2 = status_content(&state(true, Some(8123)), 3, "lattice", 0, false);
        assert_eq!(c2.plain(), "● claude · lattice :8123 · 3 conns");
    }

    #[test]
    fn empty_project_is_omitted() {
        let c = status_content(&state(true, Some(8123)), 1, "", 0, false);
        assert_eq!(c.plain(), "● claude :8123 · 1 conn");
    }

    #[test]
    fn pending_review_shows_badge() {
        let c1 = status_content(&state(true, Some(8123)), 1, "lattice", 1, false);
        assert_eq!(c1.plain(), "● claude · lattice :8123 · 1 conn · ◆ review");
        let c2 = status_content(&state(true, Some(8123)), 1, "lattice", 2, false);
        assert_eq!(c2.plain(), "● claude · lattice :8123 · 1 conn · ◆ 2 reviews");
    }

    #[test]
    fn at_mention_echo_shows_when_active() {
        // `@sent` appears while the mention echo is active, after the review
        // badge. A review + a mention can both show.
        let c = status_content(&state(true, Some(8123)), 1, "lattice", 0, true);
        assert_eq!(c.plain(), "● claude · lattice :8123 · 1 conn · @sent");
        let c2 = status_content(&state(true, Some(8123)), 1, "lattice", 1, true);
        assert_eq!(c2.plain(), "● claude · lattice :8123 · 1 conn · ◆ review · @sent");
    }

    #[test]
    fn mention_remaining_is_some_after_ping_then_none_after_window() {
        let m = MentionState::new(Arc::new(Notify::new()));
        assert!(m.remaining().is_none(), "inactive before any ping");
        m.ping();
        assert!(m.remaining().is_some(), "active right after ping");
    }

    #[test]
    fn review_guard_tracks_count_and_wakes() {
        let changed = Arc::new(Notify::new());
        let review = ReviewState::new(changed);
        let count = review.count_handle();
        assert_eq!(count.load(Ordering::Relaxed), 0);
        {
            let _g = review.begin();
            assert_eq!(count.load(Ordering::Relaxed), 1, "begin increments");
            let _g2 = review.begin();
            assert_eq!(count.load(Ordering::Relaxed), 2, "concurrent reviews count");
        }
        assert_eq!(count.load(Ordering::Relaxed), 0, "guards decrement on drop");
    }

    #[test]
    fn project_name_takes_workspace_basename() {
        assert_eq!(project_name(&["/home/me/src/lattice".to_string()]), "lattice");
        assert_eq!(project_name(&["/home/me/src/lattice/".to_string()]), "lattice");
        assert_eq!(project_name(&[]), "");
        assert_eq!(project_name(&["".to_string()]), "");
    }

    #[tokio::test]
    async fn publisher_pushes_content_for_a_registered_buffer() {
        use std::time::Duration;

        let bus = Arc::new(EventBus::new());
        let server_state = Arc::new(ArcSwap::from_pointee(state(true, Some(9001))));
        let conn_count = Arc::new(AtomicUsize::new(0));
        let ide_buffers: IdeBuffers = Arc::new(Mutex::new(HashSet::new()));
        let changed = Arc::new(Notify::new());

        // Register the buffer + subscribe BEFORE spawning, so the publisher's
        // first iteration publishes for it (no wake race in the test).
        let buf = BufferId(7);
        ide_buffers
            .lock()
            .unwrap()
            .insert(buf);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<ModelineElementUpdate>();
        bus.subscribe_typed(tx);

        let review_count = Arc::new(AtomicUsize::new(0));
        let mention = MentionState::new(Arc::new(Notify::new()));
        spawn_status_publisher(
            bus.clone(),
            server_state,
            conn_count,
            "lattice".to_string(),
            review_count,
            mention,
            ide_buffers,
            changed,
            &tokio::runtime::Handle::current(),
        );

        let update = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("an update within the timeout")
            .expect("the publisher pushed an update");
        assert_eq!(update.key, ModelineKey::Buffer(buf));
        assert_eq!(update.id, ElementId::new(STATUS_ELEMENT));
        // 0 conns → ○ waiting glyph, project shown.
        assert_eq!(update.content.plain(), "○ claude · lattice :9001");
    }
}
