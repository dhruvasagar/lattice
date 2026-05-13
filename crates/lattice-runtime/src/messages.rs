//! `*messages*` transcript types -- one historical record per
//! editor echo, a bounded ring of them, and a typed
//! `MessagePushed` event published whenever a new record
//! lands.
//!
//! Lives in `lattice-runtime` so any host-side crate that
//! cares about message stream events (TUI, GPU renderer,
//! plugin host, telemetry) can subscribe without taking a
//! dependency on the renderer crate. The wire-typed
//! `lattice_grammar::EchoLevel` rides through unchanged so
//! grammar / runtime / plugin code all read the same severity
//! enum without round-tripping through a display-typed clone.
//!
//! Plugins via the WASM Component Model subscribe through the
//! plugin host's WIT bridge -- the host wires
//! `event_bus.subscribe_typed::<MessagePushed>(...)` and
//! marshals each record into the plugin's WIT struct. The
//! Rust type's location here is purely for in-process Rust
//! consumers; plugins never import this module directly.

use lattice_grammar::EchoLevel;

/// One historical minibuffer echo. The renderer's
/// `*messages*` buffer renders these in chronological order;
/// each `set_message` call appends one record. Cheap to clone
/// (bounded text) -- producers fan the same record out to
/// every subscriber on the typed event bus.
#[derive(Debug, Clone)]
pub struct MessageRecord {
    pub timestamp: std::time::SystemTime,
    pub level: EchoLevel,
    pub text: String,
}

/// Bounded chronological ring of every echo the editor has
/// emitted. Push on every `set_message`; snapshot on demand
/// for `:messages` open and live refresh. Capacity is fixed
/// at construction; once full, the oldest entry drops to make
/// room for the newest.
#[derive(Debug, Clone)]
pub struct MessagesRing {
    records: std::collections::VecDeque<MessageRecord>,
    capacity: usize,
}

impl MessagesRing {
    /// Construct an empty ring with the given capacity. `0`
    /// is normalised via `max(1)` so push never silently
    /// drops every record.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            records: std::collections::VecDeque::new(),
            capacity: capacity.max(1),
        }
    }

    /// Append a record. Evicts the oldest entry when at
    /// capacity. O(1) amortized; the ring is bounded so a
    /// flood of messages stays at a fixed memory ceiling.
    pub fn push(&mut self, record: MessageRecord) {
        if self.records.len() == self.capacity {
            self.records.pop_front();
        }
        self.records.push_back(record);
    }

    /// Borrow the records in chronological order (oldest
    /// first). Renderers walk this for the transcript view.
    pub fn records(&self) -> &std::collections::VecDeque<MessageRecord> {
        &self.records
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl Default for MessagesRing {
    fn default() -> Self {
        // 2000 records is plenty for typical sessions: at an
        // average ~200 bytes per record the worst-case
        // footprint is ~400 KB. Users can review a morning's
        // worth of LSP progress + diagnostics without
        // hitting the wrap.
        Self::with_capacity(2000)
    }
}

/// Typed event published on the editor's event bus whenever
/// `set_message` runs. Carries the appended record (cloned
/// from the ring). Subscribers see every echo in arrival
/// order; the renderer's `*messages*` buffer live tail, the
/// plugin host's WIT bridge, future telemetry hooks, etc.
/// are all peer subscribers with no privileged path.
#[derive(Debug, Clone)]
pub struct MessagePushed {
    pub record: MessageRecord,
}

lattice_protocol::register_event!(
    MessagePushed,
    "ui.message-pushed",
    "Fired when the editor pushes a new minibuffer echo / notification \
     onto the messages ring. Subscribers receive one event per \
     `set_message` call regardless of renderer; the bounded ring on \
     `App.messages` is the source of truth for replay.",
    "lattice-runtime",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_push_appends_in_order_and_caps_capacity() {
        let mut ring = MessagesRing::with_capacity(3);
        for i in 0..5 {
            ring.push(MessageRecord {
                timestamp: std::time::SystemTime::now(),
                level: EchoLevel::Info,
                text: format!("m{i}"),
            });
        }
        assert_eq!(ring.len(), 3);
        let texts: Vec<&str> = ring.records().iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["m2", "m3", "m4"]);
    }

    #[test]
    fn ring_default_has_nonzero_capacity() {
        let r = MessagesRing::default();
        assert!(r.capacity() >= 2000);
        assert!(r.is_empty());
    }

    #[test]
    fn ring_with_zero_capacity_normalises_to_one() {
        let mut r = MessagesRing::with_capacity(0);
        assert_eq!(r.capacity(), 1);
        r.push(MessageRecord {
            timestamp: std::time::SystemTime::now(),
            level: EchoLevel::Warn,
            text: "x".into(),
        });
        assert_eq!(r.len(), 1);
    }
}
