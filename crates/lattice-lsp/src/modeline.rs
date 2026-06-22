//! LSP modeline element (ML.3c) + the shared progress/status store it is
//! built from. Produced entirely in `lattice-lsp` (the owner) and pushed
//! over the event bus.
//!
//! ## Why a forwarder task + shared store here, not host accumulation
//!
//! Decision (A): `$/progress` + `experimental/serverStatus` are LSP
//! protocol state, so their accumulation belongs in `lattice-lsp`, not
//! `dispatch.rs`. The LSP *actor* only parses and emits these as events
//! (`actor.rs`); nothing accumulates them server-side. This module owns
//! that accumulation in [`LspProgressStore`] (an `ArcSwap`-backed shared
//! handle):
//!
//! - the **forwarder task** subscribes the relevant events, folds them
//!   into the store, and publishes [`ModelineElementUpdate`] for each
//!   LSP-attached buffer — exactly as a WASM plugin would
//!   (`docs/dev/architecture/modeline.md` §6). The host's §12 wake
//!   forwarder turns each push into an off-keystroke repaint; no producer
//!   runs on the render path (paramount #1).
//! - the host reads the **same** store (via the
//!   [`LspProgressStoreHandle`] it stashes at boot, the pattern it
//!   already uses for `LspSupervisorHandle`) for `:lsp-progress-cancel`,
//!   which needs the in-flight cancellable-token list. One accumulator,
//!   two readers — no duplication, and the accumulation logic + state are
//!   out of the host.
//!
//! Per-buffer keying gates the badge to LSP buffers: content is pushed
//! keyed `Buffer(id)` for each attached buffer (the badge is process-wide
//! today, so every attached buffer gets the same value) and cleared on
//! detach. A non-LSP pane has no `lsp` content and renders nothing.
//!
//! Replaces the retired `LspMode::status_line_items` (badge) +
//! `LspProgressMode::status_line_items` (progress) pull and the host-side
//! `drain_lsp_progress_events` / `drain_lsp_server_status` accumulators.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use arc_swap::ArcSwap;
use lattice_core::BufferId;
use lattice_mode::{
    ElementContent, ElementId, ModelineElement, ModelineElementUpdate, ModelineKey, ModelineRole,
    ModelineService, Zone,
};
use lattice_runtime::EventBus;

use crate::events::{
    LspBufferAttached, LspBufferDetached, LspProgressKind, LspProgressUpdate, LspServerHealth,
    LspServerStatusChanged,
};

/// Modeline element id for the LSP readiness badge + progress. Owned by
/// `lattice-lsp` (`feedback_mode_owns_its_surface`).
pub const LSP_ELEMENT: &str = "lsp";

/// Theme role the badge spans resolve through. `ModelineRole` is a
/// string key by design (dep-inversion, see its doc); matches the
/// `modeline.mode_item` element registered in `lattice-theme` (ML.1b).
const LSP_ROLE: &str = "modeline.mode_item";

/// Register the `lsp` descriptor (ML.3c). PaneLocal (content pushed per
/// attached buffer), Right zone before `core.position` (priority 5).
pub fn register_lsp_modeline_element(svc: &ModelineService) {
    svc.register(ModelineElement::new(ElementId::new(LSP_ELEMENT), Zone::Right, 5));
}

/// The relocated LSP progress/status accumulator (decision A). The LSP
/// actor only emits `$/progress` / `serverStatus` as events; this is the
/// single place they are folded into a queryable map. Two `ArcSwap`s for
/// wait-free reads + lock-free single-writer (forwarder) updates, so the
/// host can read [`Self::progress_snapshot`] from the actor thread for
/// `:lsp-progress-cancel` while the off-actor forwarder writes.
#[derive(Debug)]
pub struct LspProgressStore {
    progress: ArcSwap<HashMap<(Arc<str>, String), LspProgressUpdate>>,
    server_status: ArcSwap<HashMap<Arc<str>, LspServerStatusChanged>>,
}

/// Shared handle to the [`LspProgressStore`] — the host stashes one at
/// boot; the forwarder writes through its clone.
pub type LspProgressStoreHandle = Arc<LspProgressStore>;

impl Default for LspProgressStore {
    fn default() -> Self {
        Self {
            progress: ArcSwap::from_pointee(HashMap::new()),
            server_status: ArcSwap::from_pointee(HashMap::new()),
        }
    }
}

impl LspProgressStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fold one `$/progress` event: Begin inserts, Report merges
    /// title/percentage from the prior entry, End removes. Relocated
    /// verbatim from the host's old `drain_lsp_progress_events`.
    pub fn apply_progress(&self, ev: LspProgressUpdate) {
        self.progress.rcu(|cur| {
            let mut next = (**cur).clone();
            let key = (ev.server_id.clone(), ev.token.clone());
            match ev.kind {
                LspProgressKind::Begin => {
                    next.insert(key, ev.clone());
                }
                LspProgressKind::Report => {
                    if let Some(prev) = next.get(&key) {
                        let title = ev.title.clone().or_else(|| prev.title.clone());
                        let merged = LspProgressUpdate {
                            server_id: ev.server_id.clone(),
                            token: ev.token.clone(),
                            kind: ev.kind,
                            title,
                            message: ev.message.clone(),
                            percentage: ev.percentage.or(prev.percentage),
                            cancellable: ev.cancellable,
                        };
                        next.insert(key, merged);
                    } else {
                        next.insert(key, ev.clone());
                    }
                }
                LspProgressKind::End => {
                    next.remove(&key);
                }
            }
            next
        });
    }

    /// Record the latest `experimental/serverStatus` for a server.
    pub fn set_server_status(&self, ev: LspServerStatusChanged) {
        self.server_status.rcu(|cur| {
            let mut next = (**cur).clone();
            next.insert(ev.server_id.clone(), ev.clone());
            next
        });
    }

    /// Wait-free snapshot of the in-flight progress map. `:lsp-progress-cancel`
    /// reads this (host/actor thread) to find cancellable tokens.
    pub fn progress_snapshot(&self) -> Arc<HashMap<(Arc<str>, String), LspProgressUpdate>> {
        self.progress.load_full()
    }

    /// Current `lsp` element content from the accumulated maps.
    fn content(&self) -> ElementContent {
        lsp_content(&self.progress.load_full(), &self.server_status.load_full())
    }
}

/// Build the `lsp` element content from progress + server-status maps.
/// Always non-empty (the badge always shows on an attached buffer):
/// `lsp ✗` on any health error, `lsp ⟳` while any server is busy / a
/// `$/progress` token is in flight, else `lsp ✓`, with the
/// highest-percentage in-flight token's `<title> NN%` appended. Glyphs
/// are BMP-safe (lsp-architecture.md §14). Pure — unit-tested.
pub fn lsp_content(
    progress: &HashMap<(Arc<str>, String), LspProgressUpdate>,
    server_status: &HashMap<Arc<str>, LspServerStatusChanged>,
) -> ElementContent {
    let indexing = progress
        .values()
        .any(|u| !matches!(u.kind, LspProgressKind::End));
    let mut any_error = false;
    let mut any_busy = false;
    for s in server_status.values() {
        if matches!(s.health, LspServerHealth::Error) {
            any_error = true;
        }
        if !s.quiescent {
            any_busy = true;
        }
    }
    let badge = if any_error {
        "lsp ✗"
    } else if indexing || any_busy {
        "lsp ⟳"
    } else {
        "lsp ✓"
    };

    // Highest-percentage in-flight token's detail (MO.4.b parity).
    let mut best: Option<&LspProgressUpdate> = None;
    for update in progress.values() {
        if matches!(update.kind, LspProgressKind::End) {
            continue;
        }
        best = Some(match best {
            None => update,
            Some(cur) => {
                if update.percentage.unwrap_or(0) >= cur.percentage.unwrap_or(0) {
                    update
                } else {
                    cur
                }
            }
        });
    }
    let mut text = badge.to_string();
    if let Some(p) = best {
        let mut detail = String::new();
        if let Some(title) = &p.title {
            detail.push_str(title);
        }
        if let Some(pct) = p.percentage {
            if !detail.is_empty() {
                detail.push(' ');
            }
            detail.push_str(&format!("{pct}%"));
        }
        if detail.is_empty() {
            detail.push_str(&p.token);
        }
        text.push(' ');
        text.push_str(&detail);
    }
    ElementContent::text(text, ModelineRole::new(LSP_ROLE))
}

/// Publish `content` for buffer `buf`'s `lsp` slot.
fn push(bus: &EventBus, buf: BufferId, content: ElementContent) {
    bus.publish_typed(ModelineElementUpdate {
        key: ModelineKey::Buffer(buf),
        id: ElementId::new(LSP_ELEMENT),
        content,
    });
}

/// Push the current badge content to every attached buffer's `lsp` slot.
fn broadcast(bus: &EventBus, store: &LspProgressStore, attached: &HashSet<BufferId>) {
    if attached.is_empty() {
        return;
    }
    let content = store.content();
    for buf in attached {
        push(bus, *buf, content.clone());
    }
}

/// Spawn the LSP modeline forwarder (ML.3c). Subscribes the four event
/// types, folds progress/status into the shared `store`, and republishes
/// the `lsp` element content per attached buffer on every change. The
/// host wires this at boot with the shared bus + the store handle it also
/// keeps for `:lsp-progress-cancel` + the LSP runtime handle. A closed
/// bus (all senders dropped) ends the task cleanly — never panics.
pub fn spawn_modeline_forwarder(
    bus: Arc<EventBus>,
    store: LspProgressStoreHandle,
    runtime: &tokio::runtime::Handle,
) {
    use tokio::sync::mpsc;
    let (prog_tx, mut prog_rx) = mpsc::unbounded_channel::<LspProgressUpdate>();
    bus.subscribe_typed(prog_tx);
    let (status_tx, mut status_rx) = mpsc::unbounded_channel::<LspServerStatusChanged>();
    bus.subscribe_typed(status_tx);
    let (attach_tx, mut attach_rx) = mpsc::unbounded_channel::<LspBufferAttached>();
    bus.subscribe_typed(attach_tx);
    let (detach_tx, mut detach_rx) = mpsc::unbounded_channel::<LspBufferDetached>();
    bus.subscribe_typed(detach_tx);

    runtime.spawn(async move {
        let mut attached: HashSet<BufferId> = HashSet::new();
        loop {
            tokio::select! {
                ev = prog_rx.recv() => match ev {
                    Some(ev) => {
                        store.apply_progress(ev);
                        broadcast(&bus, &store, &attached);
                    }
                    None => break,
                },
                ev = status_rx.recv() => match ev {
                    Some(ev) => {
                        store.set_server_status(ev);
                        broadcast(&bus, &store, &attached);
                    }
                    None => break,
                },
                ev = attach_rx.recv() => match ev {
                    Some(ev) => {
                        let buf = BufferId(ev.id.raw() as u32);
                        attached.insert(buf);
                        // Seed the freshly-attached buffer with current state.
                        push(&bus, buf, store.content());
                    }
                    None => break,
                },
                ev = detach_rx.recv() => match ev {
                    Some(ev) => {
                        let buf = BufferId(ev.id.raw() as u32);
                        attached.remove(&buf);
                        // Empty content clears the slot (badge disappears).
                        push(&bus, buf, ElementContent::default());
                    }
                    None => break,
                },
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prog(
        server: &str,
        token: &str,
        kind: LspProgressKind,
        title: Option<&str>,
        pct: Option<u32>,
    ) -> LspProgressUpdate {
        LspProgressUpdate {
            server_id: Arc::from(server),
            token: token.to_string(),
            kind,
            title: title.map(String::from),
            message: None,
            percentage: pct,
            cancellable: false,
        }
    }

    fn status(server: &str, quiescent: bool, health: LspServerHealth) -> LspServerStatusChanged {
        LspServerStatusChanged {
            server_id: Arc::from(server),
            quiescent,
            health,
            message: None,
        }
    }

    /// Quiescent, no progress → `lsp ✓`.
    #[test]
    fn badge_ok_when_quiescent() {
        let mut ss = HashMap::new();
        ss.insert(Arc::<str>::from("rust"), status("rust", true, LspServerHealth::Ok));
        assert_eq!(lsp_content(&HashMap::new(), &ss).plain(), "lsp ✓");
    }

    /// Non-quiescent server OR in-flight progress → `lsp ⟳`, with the
    /// highest-percentage token's `<title> NN%` appended.
    #[test]
    fn badge_busy_with_progress_detail() {
        let mut p = HashMap::new();
        p.insert(
            (Arc::<str>::from("rust"), "tok1".to_string()),
            prog("rust", "tok1", LspProgressKind::Report, Some("indexing"), Some(40)),
        );
        p.insert(
            (Arc::<str>::from("rust"), "tok2".to_string()),
            prog("rust", "tok2", LspProgressKind::Begin, Some("building"), Some(80)),
        );
        // Highest percentage (80%) wins the detail.
        assert_eq!(lsp_content(&p, &HashMap::new()).plain(), "lsp ⟳ building 80%");
    }

    /// Health error wins over busy/quiescent → `lsp ✗`.
    #[test]
    fn badge_error_takes_priority() {
        let mut ss = HashMap::new();
        ss.insert(Arc::<str>::from("rust"), status("rust", false, LspServerHealth::Error));
        assert_eq!(lsp_content(&HashMap::new(), &ss).plain(), "lsp ✗");
    }

    /// The store folds Begin/Report/End: merges title + percentage,
    /// removes ended tokens; the cancel-facing snapshot reflects it.
    #[test]
    fn store_folds_progress_and_snapshots() {
        let store = LspProgressStore::new();
        store.apply_progress(prog("rust", "t", LspProgressKind::Begin, Some("scan"), None));
        // Report without a title keeps the Begin title; adds percentage.
        store.apply_progress(prog("rust", "t", LspProgressKind::Report, None, Some(50)));
        let snap = store.progress_snapshot();
        let entry = snap.get(&(Arc::<str>::from("rust"), "t".to_string())).unwrap();
        assert_eq!(entry.title.as_deref(), Some("scan"));
        assert_eq!(entry.percentage, Some(50));
        // The badge reflects the in-flight token (busy).
        assert_eq!(store.content().plain(), "lsp ⟳ scan 50%");
        // End removes it; snapshot empties.
        store.apply_progress(prog("rust", "t", LspProgressKind::End, None, None));
        assert!(store.progress_snapshot().is_empty());
    }

    /// The `lsp` descriptor is Right zone, priority 5 (before
    /// `core.position` at 10).
    #[test]
    fn register_lsp_element_is_right_zone() {
        let svc = ModelineService::new();
        register_lsp_modeline_element(&svc);
        let snap = svc.snapshot();
        let el = snap.registry.get(&ElementId::new(LSP_ELEMENT)).expect("lsp descriptor");
        assert_eq!(el.zone, Zone::Right);
        assert_eq!(el.priority, 5);
    }
}
