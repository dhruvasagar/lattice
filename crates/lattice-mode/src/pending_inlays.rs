//! DL.3b: mode-published inline virtual text.
//!
//! The inlay peer of [`crate::pending_synthetic_highlights`], and it
//! exists for the same reason: buffer-locals are written host-side, so
//! a mode living in its own crate cannot reach one directly. It pushes
//! here instead, and the Editor drains the map into the buffer's
//! `ExtraInlays` local on the next tick.
//!
//! Deliberately generic. Nothing here knows about listings — a mode
//! that wants leading icons, a plugin that wants trailing annotations,
//! and any future producer of inline virtual text use the same channel.
//! The LSP hint path is unchanged and merges beside these.
//!
//! `store_and_wake` fires the waker, so a producer that lands off the
//! actor thread reaches the screen **without a keypress** — the failure
//! mode `feedback_async_needs_wake` records, designed out at the seam
//! rather than left to each caller to remember.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lattice_cells::Style;
use lattice_core::BufferId;

/// One piece of inline virtual text: where it anchors, what it says,
/// and how to paint it.
///
/// Mirrors the host's `InlayHintRow` without depending on it —
/// `lattice-mode` sits below `lattice-host`, so the host converts on
/// drain. `style` is normally [`Style::Element`] naming an element the
/// producing mode registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlayRow {
    /// 0-based buffer line.
    pub line: u32,
    /// 0-based utf-8 byte offset within that line. `0` anchors the
    /// text at the start of the row (a leading icon).
    pub byte: u32,
    /// The virtual text itself.
    pub text: String,
    /// How to paint it.
    pub style: Style,
}

/// Shared state between mode-side producers and the Editor's tick
/// drain. A buffer's entry is a **full replacement** — a producer that
/// recomputes a listing publishes the whole set, so a shorter listing
/// leaves nothing of the old behind.
pub struct PendingInlays {
    pub map: Arc<Mutex<HashMap<BufferId, Vec<InlayRow>>>>,
    pub waker: Arc<Mutex<Option<Arc<tokio::sync::Notify>>>>,
}

/// Shared-handle alias. Registered and looked up under **this** type,
/// per the `ServiceRegistry` TypeId rule — registering an
/// `Arc<PendingInlays>` and asking for `PendingInlays` silently returns
/// `None`.
pub type PendingInlaysHandle = Arc<PendingInlays>;

impl Default for PendingInlays {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingInlays {
    pub fn new() -> Self {
        Self {
            map: Arc::new(Mutex::new(HashMap::new())),
            waker: Arc::new(Mutex::new(None)),
        }
    }

    /// Publish `rows` for `buffer_id`, replacing any previous set, and
    /// wake the Editor so they reach the screen without waiting for a
    /// keystroke.
    pub fn store_and_wake(&self, buffer_id: BufferId, rows: Vec<InlayRow>) {
        if let Ok(mut map) = self.map.lock() {
            map.insert(buffer_id, rows);
        }
        self.fire_waker();
    }

    /// Drop a buffer's virtual text (buffer closed, or the producing
    /// mode deactivated). Wakes, so the removal is painted promptly.
    pub fn clear_and_wake(&self, buffer_id: BufferId) {
        if let Ok(mut map) = self.map.lock() {
            map.remove(&buffer_id);
        }
        self.fire_waker();
    }

    pub fn set_waker(&self, waker: Arc<tokio::sync::Notify>) {
        if let Ok(mut w) = self.waker.lock() {
            *w = Some(waker);
        }
    }

    fn fire_waker(&self) {
        if let Ok(w) = self.waker.lock()
            && let Some(n) = w.as_ref()
        {
            n.notify_one();
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn row(line: u32, text: &str) -> InlayRow {
        InlayRow {
            line,
            byte: 0,
            text: text.to_string(),
            style: Style::InlayHint,
        }
    }

    #[test]
    fn store_replaces_rather_than_appends() {
        let p = PendingInlays::new();
        let b = BufferId(1);
        p.store_and_wake(b, vec![row(0, "a"), row(1, "b")]);
        p.store_and_wake(b, vec![row(0, "c")]);
        let map = p.map.lock().unwrap();
        assert_eq!(
            map.get(&b).unwrap().len(),
            1,
            "a recomputed listing publishes the whole set; a shorter one \
             must not leave the old rows behind"
        );
    }

    #[test]
    fn clear_removes_the_buffers_rows() {
        let p = PendingInlays::new();
        let b = BufferId(1);
        p.store_and_wake(b, vec![row(0, "a")]);
        p.clear_and_wake(b);
        assert!(p.map.lock().unwrap().get(&b).is_none());
    }

    #[tokio::test]
    async fn store_wakes_without_a_keystroke() {
        let p = PendingInlays::new();
        let notify = Arc::new(tokio::sync::Notify::new());
        p.set_waker(notify.clone());

        let waited = notify.notified();
        p.store_and_wake(BufferId(1), vec![row(0, "a")]);
        // `notify_one` before `.await` is remembered, so this resolves.
        tokio::time::timeout(std::time::Duration::from_secs(5), waited)
            .await
            .expect(
                "store_and_wake must fire the waker — without it the rows sit \
                     until the user happens to press a key",
            );
    }
}
