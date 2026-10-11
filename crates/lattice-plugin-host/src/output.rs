//! LH.0.5 — plugin output buffers: the store a plugin writes lines into, and
//! the event that tells a view they arrived.
//!
//! Design: `docs/dev/architecture/lighthouse.md` §3.5.
//!
//! A plugin that runs a job has somewhere to *start* it and somewhere to hear
//! about it, and until this nowhere to show it: an events handler returns
//! nothing, so it cannot open a buffer or put text in one. This is the missing
//! half, and it is the shape every native streaming buffer already has —
//! `*compilation*`, `*messages*`, the LSP logs, `*plugin-trace*`:
//!
//! ```text
//! producer ──► store (bounded ring) ──► typed event on the bus ──► the mode
//!                                                                  that owns
//!                                                                  the buffer
//! ```
//!
//! The producer never touches a buffer. It appends to a named ring here and
//! the store publishes [`PluginOutputPushed`]; `plugin-output-mode` (in
//! `lattice-plugin-trace`), activated on a buffer of that name, seeds itself
//! from the ring and tails the event off-thread. So output written before
//! anyone opened the buffer is not lost, and output written while nobody is
//! looking costs a ring push.
//!
//! ## Why a store rather than only an event
//!
//! An event with no retained state would reach only a buffer that is already
//! open. An install started from a command whose buffer the user closed, or
//! one that began before the open effect was applied, would show nothing.
//!
//! ## Seed and tail without a gap or a repeat
//!
//! A view subscribes first and snapshots second, so nothing falls between the
//! two — which means a line can be in *both*. Every line therefore has a
//! position: `epoch` names one page of the buffer — it changes on every reset
//! — and `seq` counts lines since that page began. A snapshot reports where it
//! ends, an event reports where it starts, and [`Tail`] drops exactly the
//! overlap.
//!
//! Epochs come from one counter for the whole store, not one per buffer, so
//! they only ever go up — including across a plugin unload, which drops the
//! plugin's buffers. A view left open over a dropped buffer then sees the
//! reloaded plugin's first line arrive in a *later* epoch and starts a clean
//! page, where a per-buffer count restarting at zero would have looked like
//! lines it had already shown.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Lines kept per buffer. Older lines fall off the front.
pub const MAX_LINES: usize = 10_000;
/// Lines accepted from one `output-append` call; the rest are dropped, with a
/// marker line saying how many.
pub const MAX_LINES_PER_CALL: usize = 1024;
/// Characters kept per line; a longer line is cut, with an ellipsis.
pub const MAX_LINE_CHARS: usize = 4096;
/// Buffers one plugin may hold. A plugin that names its buffers after its
/// inputs (`*lsp-install:<server>*`) must not be able to grow the store
/// without bound.
pub const MAX_BUFFERS_PER_PLUGIN: usize = 32;

/// What an output buffer says in its headerline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputState {
    /// Work is in flight.
    Running,
    /// It finished and worked.
    Succeeded,
    /// It finished and did not.
    Failed,
}

/// The headerline of one output buffer: a state and a short line of text
/// (`downloading rust-analyzer… 43%`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutputStatus {
    pub state: OutputState,
    pub text: String,
}

/// One change to an output buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutputChange {
    /// `lines` were appended; the first of them is line `first_seq` of the
    /// current epoch.
    Append { first_seq: u64, lines: Vec<String> },
    /// The headerline changed.
    Status(OutputStatus),
    /// The buffer was emptied; this is the start of `epoch`.
    Reset,
}

/// Fired for every change to a plugin output buffer. The views subscribe and
/// drain it off-thread — the `PluginTracePushed` precedent.
#[derive(Clone, Debug)]
pub struct PluginOutputPushed {
    /// The buffer's synthetic name (`*lsp-install:rust-analyzer*`).
    pub name: Arc<str>,
    /// Which page of the buffer this change belongs to (see the module doc).
    pub epoch: u64,
    pub change: OutputChange,
}

lattice_protocol::register_event!(
    PluginOutputPushed,
    "plugin.output-pushed",
    "Fired when a plugin appends to, resets, or re-labels one of its output buffers.",
    "lattice-plugin-host",
);

/// A buffer as a view finds it on opening.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputSnapshot {
    pub epoch: u64,
    /// Sequence number of the line *after* the last one in `lines`.
    pub next_seq: u64,
    pub lines: Vec<String>,
    pub status: Option<OutputStatus>,
}

struct Entry {
    /// The plugin that first wrote here, by manifest name — stable across a
    /// reload, where a host-issued id is not.
    owner: String,
    epoch: u64,
    next_seq: u64,
    lines: VecDeque<String>,
    status: Option<OutputStatus>,
}

type OutputPublisher = Box<dyn Fn(PluginOutputPushed) + Send + Sync>;

/// The store. One per editor, registered as [`PluginOutputHandle`].
#[derive(Default)]
pub struct PluginOutput {
    buffers: Mutex<HashMap<Arc<str>, Entry>>,
    publisher: Mutex<Option<OutputPublisher>>,
    /// The next epoch to hand out. Store-wide; see the module doc.
    next_epoch: AtomicU64,
}

/// Shared handle alias (ServiceRegistry Arc/TypeId rule: register **and** look
/// up as `PluginOutputHandle`).
pub type PluginOutputHandle = Arc<PluginOutput>;

/// A poisoned lock here means a publisher panicked mid-push; the data is a log
/// and is still worth reading.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// A buffer name a plugin may claim: the `*name*` shape every synthetic buffer
/// has, on one line, and not so long it is a payload.
fn check_name(name: &str) -> Result<(), String> {
    let shaped = name.len() > 2 && name.starts_with('*') && name.ends_with('*');
    if !shaped || name.len() > 128 || name.chars().any(char::is_control) {
        return Err(format!(
            "'{}' is not an output buffer name — use the `*name*` form, on one line, \
             at most 128 bytes",
            name.escape_debug()
        ));
    }
    Ok(())
}

/// Guest strings as buffer lines: split on newlines (a guest that sends
/// `"a\nb"` meant two lines, and a raw newline would desynchronise `seq` from
/// the buffer), carriage returns dropped, each cut to [`MAX_LINE_CHARS`].
fn clean(lines: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(lines.len());
    let mut dropped = 0usize;
    for raw in &lines {
        for piece in raw.split('\n') {
            if out.len() >= MAX_LINES_PER_CALL {
                dropped += 1;
                continue;
            }
            let piece = piece.replace('\r', "");
            if piece.chars().count() > MAX_LINE_CHARS {
                let mut cut: String = piece.chars().take(MAX_LINE_CHARS).collect();
                cut.push('\u{2026}');
                out.push(cut);
            } else {
                out.push(piece);
            }
        }
    }
    if dropped > 0 {
        out.push(format!("\u{2026} {dropped} more lines dropped"));
    }
    out
}

impl PluginOutput {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install / replace the event publisher (wired at boot to the runtime bus).
    pub fn set_event_publisher(&self, publisher: OutputPublisher) {
        *lock(&self.publisher) = Some(publisher);
    }

    fn publish(&self, name: &Arc<str>, epoch: u64, change: OutputChange) {
        if let Some(publisher) = lock(&self.publisher).as_ref() {
            publisher(PluginOutputPushed {
                name: name.clone(),
                epoch,
                change,
            });
        }
    }

    /// Run `f` on `plugin`'s buffer `name`, creating it if this is the first
    /// write. `Err` when the name is malformed, belongs to another plugin, or
    /// would take `plugin` past [`MAX_BUFFERS_PER_PLUGIN`].
    fn with_entry<R>(
        &self,
        plugin: &str,
        name: &str,
        f: impl FnOnce(&mut Entry) -> R,
    ) -> Result<(Arc<str>, R), String> {
        check_name(name)?;
        let mut buffers = lock(&self.buffers);
        let existing = buffers.get_key_value(name).map(|(key, entry)| {
            if entry.owner == plugin {
                Ok(key.clone())
            } else {
                Err(format!(
                    "output buffer '{name}' belongs to plugin '{}'",
                    entry.owner
                ))
            }
        });
        let key = match existing {
            Some(found) => found?,
            None => {
                let held = buffers.values().filter(|e| e.owner == plugin).count();
                if held >= MAX_BUFFERS_PER_PLUGIN {
                    return Err(format!(
                        "plugin '{plugin}' already holds {MAX_BUFFERS_PER_PLUGIN} output \
                         buffers; '{name}' was not created"
                    ));
                }
                Arc::from(name)
            }
        };
        let entry = buffers.entry(key.clone()).or_insert_with(|| Entry {
            owner: plugin.to_string(),
            epoch: self.next_epoch.fetch_add(1, Ordering::Relaxed),
            next_seq: 0,
            lines: VecDeque::new(),
            status: None,
        });
        Ok((key, f(entry)))
    }

    /// Append `lines` to `plugin`'s buffer `name`.
    pub fn append(&self, plugin: &str, name: &str, lines: Vec<String>) -> Result<(), String> {
        let lines = clean(lines);
        if lines.is_empty() {
            // Still validates the name and claims the buffer, so an empty
            // first write fails the same way a full one would.
            return self.with_entry(plugin, name, |_| ()).map(|_| ());
        }
        let (key, (epoch, first_seq)) = self.with_entry(plugin, name, |entry| {
            let first_seq = entry.next_seq;
            entry.next_seq += lines.len() as u64;
            entry.lines.extend(lines.iter().cloned());
            while entry.lines.len() > MAX_LINES {
                entry.lines.pop_front();
            }
            (entry.epoch, first_seq)
        })?;
        self.publish(&key, epoch, OutputChange::Append { first_seq, lines });
        Ok(())
    }

    /// Set the headerline of `plugin`'s buffer `name`.
    pub fn set_status(&self, plugin: &str, name: &str, status: OutputStatus) -> Result<(), String> {
        let status = OutputStatus {
            state: status.state,
            text: clean(vec![status.text])
                .into_iter()
                .next()
                .unwrap_or_default(),
        };
        let (key, epoch) = self.with_entry(plugin, name, |entry| {
            entry.status = Some(status.clone());
            entry.epoch
        })?;
        self.publish(&key, epoch, OutputChange::Status(status));
        Ok(())
    }

    /// Empty `plugin`'s buffer `name` and clear its headerline — a re-run
    /// starts on a clean page.
    pub fn reset(&self, plugin: &str, name: &str) -> Result<(), String> {
        let (key, epoch) = self.with_entry(plugin, name, |entry| {
            entry.epoch = self.next_epoch.fetch_add(1, Ordering::Relaxed);
            entry.next_seq = 0;
            entry.lines.clear();
            entry.status = None;
            entry.epoch
        })?;
        self.publish(&key, epoch, OutputChange::Reset);
        Ok(())
    }

    /// What a view opening `name` starts from. `None` when no plugin has
    /// written there.
    pub fn snapshot(&self, name: &str) -> Option<OutputSnapshot> {
        let buffers = lock(&self.buffers);
        let entry = buffers.get(name)?;
        Some(OutputSnapshot {
            epoch: entry.epoch,
            next_seq: entry.next_seq,
            lines: entry.lines.iter().cloned().collect(),
            status: entry.status.clone(),
        })
    }

    /// Drop every buffer `plugin` holds — for an unload. A buffer still open
    /// on screen keeps the text it has; if the plugin comes back and writes
    /// there again, that is a later epoch and the view starts a clean page.
    pub fn forget_plugin(&self, plugin: &str) {
        lock(&self.buffers).retain(|_, entry| entry.owner != plugin);
    }
}

/// One thing a view does to its buffer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TailStep {
    /// Append this text (newline-terminated) to the buffer.
    Append(String),
    /// Empty the buffer and clear its headerline.
    Clear,
    /// Show this headerline.
    Status(OutputStatus),
}

/// A view's position in its buffer: the reconciliation between the snapshot
/// it seeded from and the events it tails. See the module doc.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tail {
    epoch: u64,
    next_seq: u64,
}

impl Tail {
    /// Start where `snapshot` ends.
    pub fn after(snapshot: &OutputSnapshot) -> Self {
        Self {
            epoch: snapshot.epoch,
            next_seq: snapshot.next_seq,
        }
    }

    /// Fold one event in. Empty when the event is already shown, or is from
    /// before the last reset.
    pub fn step(&mut self, epoch: u64, change: OutputChange) -> Vec<TailStep> {
        if epoch < self.epoch {
            return Vec::new();
        }
        let mut steps = Vec::new();
        if epoch > self.epoch {
            // A later epoch always begins with its `Reset`; if that one was
            // somehow not seen, the buffer is stale all the same, so whatever
            // arrives first from the new epoch clears it.
            self.epoch = epoch;
            self.next_seq = 0;
            steps.push(TailStep::Clear);
        }
        match change {
            OutputChange::Reset => {}
            OutputChange::Status(status) => steps.push(TailStep::Status(status)),
            OutputChange::Append { first_seq, lines } => {
                let end = first_seq + lines.len() as u64;
                if end > self.next_seq {
                    let seen = self.next_seq.saturating_sub(first_seq) as usize;
                    self.next_seq = end;
                    let mut text = String::new();
                    for line in lines.iter().skip(seen) {
                        text.push_str(line);
                        text.push('\n');
                    }
                    steps.push(TailStep::Append(text));
                }
            }
        }
        steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recording() -> (PluginOutput, Arc<Mutex<Vec<PluginOutputPushed>>>) {
        let out = PluginOutput::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        out.set_event_publisher(Box::new(move |ev| lock(&sink).push(ev)));
        (out, seen)
    }

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn appended_lines_are_kept_and_published_in_order() {
        let (out, seen) = recording();
        out.append("lh", "*log*", lines(&["a", "b"])).unwrap();
        out.append("lh", "*log*", lines(&["c"])).unwrap();

        let snap = out.snapshot("*log*").unwrap();
        assert_eq!(snap.lines, lines(&["a", "b", "c"]));
        assert_eq!(snap.next_seq, 3);

        let seen = lock(&seen);
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[1].change,
            OutputChange::Append {
                first_seq: 2,
                lines: lines(&["c"])
            }
        );
    }

    #[test]
    fn a_buffer_nobody_wrote_has_no_snapshot() {
        assert!(PluginOutput::new().snapshot("*log*").is_none());
    }

    #[test]
    fn a_name_that_is_not_a_synthetic_buffer_name_is_refused() {
        let out = PluginOutput::new();
        for bad in ["log", "*", "**", "*a\nb*", "src/main.rs"] {
            let err = out.append("lh", bad, lines(&["x"])).unwrap_err();
            assert!(err.contains("not an output buffer name"), "{bad}: {err}");
        }
        let long = format!("*{}*", "x".repeat(200));
        assert!(out.append("lh", &long, lines(&["x"])).is_err());
    }

    #[test]
    fn one_plugin_cannot_write_into_anothers_buffer() {
        let out = PluginOutput::new();
        out.append("lh", "*log*", lines(&["mine"])).unwrap();
        let err = out
            .append("other", "*log*", lines(&["theirs"]))
            .unwrap_err();
        assert!(err.contains("belongs to plugin 'lh'"), "{err}");
        assert!(out.reset("other", "*log*").is_err());
        assert_eq!(out.snapshot("*log*").unwrap().lines, lines(&["mine"]));
    }

    #[test]
    fn a_plugin_is_capped_on_how_many_buffers_it_holds() {
        let out = PluginOutput::new();
        for i in 0..MAX_BUFFERS_PER_PLUGIN {
            out.append("lh", &format!("*log:{i}*"), lines(&["x"]))
                .unwrap();
        }
        let err = out.append("lh", "*one-more*", lines(&["x"])).unwrap_err();
        assert!(err.contains("already holds"), "{err}");
        assert!(out.snapshot("*one-more*").is_none());
        // An existing buffer is still writable, and another plugin is unaffected.
        out.append("lh", "*log:0*", lines(&["y"])).unwrap();
        out.append("other", "*theirs*", lines(&["x"])).unwrap();
    }

    #[test]
    fn the_ring_drops_the_oldest_lines_and_keeps_counting() {
        let out = PluginOutput::new();
        for chunk in 0..(MAX_LINES / 1000 + 2) {
            let batch: Vec<String> = (0..1000).map(|i| format!("{chunk}:{i}")).collect();
            out.append("lh", "*log*", batch).unwrap();
        }
        let snap = out.snapshot("*log*").unwrap();
        assert_eq!(snap.lines.len(), MAX_LINES);
        assert_eq!(snap.next_seq, (MAX_LINES as u64) + 2000);
        assert_eq!(snap.lines[0], "2:0", "the first two chunks fell off");
    }

    #[test]
    fn embedded_newlines_become_lines_and_long_lines_are_cut() {
        let out = PluginOutput::new();
        let long = "x".repeat(MAX_LINE_CHARS + 10);
        out.append("lh", "*log*", vec!["a\r\nb".to_string(), long])
            .unwrap();
        let snap = out.snapshot("*log*").unwrap();
        assert_eq!(snap.lines[0], "a");
        assert_eq!(snap.lines[1], "b");
        assert_eq!(snap.lines[2].chars().count(), MAX_LINE_CHARS + 1);
        assert!(snap.lines[2].ends_with('\u{2026}'));
        assert_eq!(
            snap.next_seq, 3,
            "seq counts buffer lines, not guest strings"
        );
    }

    #[test]
    fn one_call_cannot_append_without_bound() {
        let out = PluginOutput::new();
        let flood: Vec<String> = (0..MAX_LINES_PER_CALL + 50)
            .map(|i| i.to_string())
            .collect();
        out.append("lh", "*log*", flood).unwrap();
        let snap = out.snapshot("*log*").unwrap();
        assert_eq!(snap.lines.len(), MAX_LINES_PER_CALL + 1);
        assert_eq!(snap.lines.last().unwrap(), "\u{2026} 50 more lines dropped");
    }

    #[test]
    fn reset_empties_the_buffer_and_starts_a_new_epoch() {
        let (out, seen) = recording();
        out.append("lh", "*log*", lines(&["old"])).unwrap();
        out.set_status(
            "lh",
            "*log*",
            OutputStatus {
                state: OutputState::Failed,
                text: "boom".into(),
            },
        )
        .unwrap();
        out.reset("lh", "*log*").unwrap();
        out.append("lh", "*log*", lines(&["new"])).unwrap();

        let snap = out.snapshot("*log*").unwrap();
        assert_eq!(snap.epoch, 1);
        assert_eq!(snap.lines, lines(&["new"]));
        assert_eq!(snap.next_seq, 1);
        assert_eq!(snap.status, None);

        let seen = lock(&seen);
        assert_eq!(seen[2].change, OutputChange::Reset);
        assert_eq!(seen[2].epoch, 1);
        assert_eq!(seen[3].epoch, 1);
    }

    #[test]
    fn forgetting_a_plugin_drops_only_its_buffers() {
        let out = PluginOutput::new();
        out.append("lh", "*a*", lines(&["x"])).unwrap();
        out.append("other", "*b*", lines(&["x"])).unwrap();
        out.forget_plugin("lh");
        assert!(out.snapshot("*a*").is_none());
        assert!(out.snapshot("*b*").is_some());
        // The name is free again.
        out.append("other", "*a*", lines(&["x"])).unwrap();
    }

    #[test]
    fn a_buffer_written_again_after_an_unload_is_a_later_epoch() {
        // A view left open across a reload holds the old page's position. If
        // the new page reused epoch 0 its first lines would look already-shown.
        let out = PluginOutput::new();
        out.append("lh", "*log*", lines(&["before", "the", "reload"]))
            .unwrap();
        let old = out.snapshot("*log*").unwrap();
        let mut view = Tail::after(&old);

        out.forget_plugin("lh");
        let (out, seen) = {
            let seen = Arc::new(Mutex::new(Vec::new()));
            let sink = seen.clone();
            out.set_event_publisher(Box::new(move |ev| lock(&sink).push(ev)));
            (out, seen)
        };
        out.append("lh", "*log*", lines(&["after"])).unwrap();

        let pushed = lock(&seen)[0].clone();
        assert!(pushed.epoch > old.epoch);
        assert_eq!(
            view.step(pushed.epoch, pushed.change),
            vec![TailStep::Clear, TailStep::Append("after\n".into())]
        );
    }

    fn append(first_seq: u64, items: &[&str]) -> OutputChange {
        OutputChange::Append {
            first_seq,
            lines: lines(items),
        }
    }

    fn at(epoch: u64, next_seq: u64) -> Tail {
        Tail::after(&OutputSnapshot {
            epoch,
            next_seq,
            ..Default::default()
        })
    }

    #[test]
    fn a_tail_appends_what_follows_its_snapshot() {
        let mut tail = at(0, 2);
        assert_eq!(
            tail.step(0, append(2, &["c", "d"])),
            vec![TailStep::Append("c\nd\n".into())]
        );
        assert_eq!(
            tail.step(0, append(4, &["e"])),
            vec![TailStep::Append("e\n".into())]
        );
    }

    #[test]
    fn a_tail_drops_lines_its_snapshot_already_had() {
        // Subscribed, then snapshotted: the event for lines 0..2 is in the
        // channel AND in the snapshot.
        let mut tail = at(0, 2);
        assert!(tail.step(0, append(0, &["a", "b"])).is_empty());
        // Straddling: one line seen, one new.
        assert_eq!(
            tail.step(0, append(1, &["b", "c"])),
            vec![TailStep::Append("c\n".into())]
        );
    }

    #[test]
    fn a_tail_ignores_events_from_before_the_last_reset() {
        let mut tail = at(3, 0);
        assert!(tail.step(2, append(0, &["stale"])).is_empty());
        assert!(tail.step(2, OutputChange::Reset).is_empty());
    }

    #[test]
    fn a_reset_clears_and_restarts_the_count() {
        let mut tail = at(0, 5);
        assert_eq!(tail.step(1, OutputChange::Reset), vec![TailStep::Clear]);
        assert_eq!(
            tail.step(1, append(0, &["a"])),
            vec![TailStep::Append("a\n".into())]
        );
    }

    #[test]
    fn a_missed_reset_is_implied_by_the_next_epoch_and_loses_nothing() {
        let mut tail = at(0, 5);
        assert_eq!(
            tail.step(1, append(0, &["a"])),
            vec![TailStep::Clear, TailStep::Append("a\n".into())]
        );
    }
}
