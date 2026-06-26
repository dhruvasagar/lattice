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

use crate::server::ServerState;

/// Modeline element id — owned by `lattice-claude-code`
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
pub fn status_content(state: &ServerState, conns: usize, project: &str) -> ElementContent {
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
    let text = format!("{glyph} claude{proj} :{port}{conn}");
    ElementContent::text(text, ModelineRole::new(ROLE_MODE_ITEM))
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
    ide_buffers: IdeBuffers,
    changed: Arc<Notify>,
    rt: &tokio::runtime::Handle,
) {
    rt.spawn(async move {
        let id = ElementId::new(STATUS_ELEMENT);
        let mut last: HashMap<BufferId, ElementContent> = HashMap::new();
        loop {
            let content =
                status_content(&state.load(), conn_count.load(Ordering::Relaxed), &project);
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

            changed.notified().await;
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
        assert!(status_content(&state(false, None), 0, "lattice").is_empty());
    }

    #[test]
    fn running_server_shows_glyph_project_port_and_conn_count() {
        // ○ = waiting (no agent), ● = connected. Project after `claude`.
        let c = status_content(&state(true, Some(8123)), 0, "lattice");
        assert_eq!(c.plain(), "○ claude · lattice :8123");
        let c1 = status_content(&state(true, Some(8123)), 1, "lattice");
        assert_eq!(c1.plain(), "● claude · lattice :8123 · 1 conn");
        let c2 = status_content(&state(true, Some(8123)), 3, "lattice");
        assert_eq!(c2.plain(), "● claude · lattice :8123 · 3 conns");
    }

    #[test]
    fn empty_project_is_omitted() {
        let c = status_content(&state(true, Some(8123)), 1, "");
        assert_eq!(c.plain(), "● claude :8123 · 1 conn");
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

        spawn_status_publisher(
            bus.clone(),
            server_state,
            conn_count,
            "lattice".to_string(),
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
