//! LH.0.5 — `plugin-output-mode`: the major mode of a plugin's output buffer.
//!
//! Design: `docs/dev/architecture/lighthouse.md` §3.5.
//!
//! A plugin writes lines into the host's [`PluginOutput`] store under a buffer
//! name; this mode, activated on a buffer of that name, is what puts them on
//! screen. It is `plugin-trace-mode`'s shape exactly — seed from the store,
//! subscribe to the pushed event, drain off-thread — with two additions the
//! trace view does not need:
//!
//! * a **headerline**, because an output buffer reports work in flight and
//!   async-buffer status belongs in the view header, not a status line;
//! * an exact seed/tail join ([`Tail`]), because a repeated line is noise in a
//!   trace and a lie in an install log.
//!
//! The plugin opens the buffer with the ordinary `open-synthetic-buffer`
//! effect naming this mode. Nothing here knows which plugin, or what the
//! lines mean.
//!
//! [`PluginOutput`]: lattice_plugin_host::output::PluginOutput

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

use lattice_cells::{Cell, Headerline, HeaderlineProvider, HeaderlineRow, VirtualRowProvider};
use lattice_mode::inbound::InboundBus;
use lattice_mode::{
    BufferStoreHandle, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet, Subscription, VirtualRowRegistrar,
};
use lattice_plugin_host::output::{
    OutputState, OutputStatus, PluginOutputHandle, PluginOutputPushed, Tail, TailStep,
};
use lattice_protocol::edit::Edit;
use lattice_protocol::position::{Position, Range};
use lattice_runtime::Document;
use lattice_theme::{ColorRef, ElementId, ElementName, ElementOwner, StyleSpec};

use crate::mode::append_text;

/// Canonical id of the output buffers' major mode — what a plugin names in
/// `open-synthetic-buffer`.
pub const OUTPUT_MODE_ID: &str = "plugin-output-mode";

/// Provider id tag for the headerline so a re-activation can
/// `unregister` / `register` idempotently.
const OUTPUT_HEADERLINE_PROVIDER_ID: u64 = 0x706c_6f75_7468_6c00; // "plouthl"

/// Sent after the drain has written to a buffer, so the write is painted
/// without a keypress. Carries nothing: the wake is the message.
#[derive(Clone, Copy, Debug)]
pub struct OutputLanded;

/// The wake as a service (ServiceRegistry Arc/TypeId rule: register **and**
/// look up under this alias).
pub type OutputWakeHandle = Arc<InboundBus<OutputLanded>>;

/// The `*…*` output buffers' major mode.
pub struct PluginOutputMode;

impl PluginOutputMode {
    pub fn mode_id() -> ModeId {
        ModeId::new(OUTPUT_MODE_ID)
    }
}

/// Foregrounds the headerline paints with, resolved once at activation.
#[derive(Clone, Copy)]
struct Colors {
    running: u32,
    succeeded: u32,
    failed: u32,
    text: u32,
}

const FALLBACK: Colors = Colors {
    running: 0x999999,
    succeeded: 0x44cc88,
    failed: 0xff4444,
    text: 0xcccccc,
};

/// The headerline: ` ⟳ downloading… 43% `, ` ✔ installed `, ` ✗ checksum
/// mismatch `. Absent until the plugin sets a status.
struct OutputHeaderline {
    status: Arc<RwLock<Option<OutputStatus>>>,
    version: Arc<AtomicU64>,
    colors: Colors,
}

impl Headerline for OutputHeaderline {
    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    fn render(&self) -> Option<HeaderlineRow> {
        let status = self.status.read().ok()?.clone()?;
        // Same three glyphs as the compilation headerline: all BMP, all one
        // cell wide, so the row does not shift between states.
        let (icon, icon_fg) = match status.state {
            OutputState::Running => ('\u{27f3}', self.colors.running),
            OutputState::Succeeded => ('\u{2714}', self.colors.succeeded),
            OutputState::Failed => ('\u{2717}', self.colors.failed),
        };
        let mut cells = vec![
            Cell::new(' ' as u32, icon_fg, 0, 0),
            Cell::new(icon as u32, icon_fg, 0, 0),
            Cell::new(' ' as u32, icon_fg, 0, 0),
        ];
        cells.extend(
            status
                .text
                .chars()
                .map(|c| Cell::new(c as u32, self.colors.text, 0, 0)),
        );
        Some(HeaderlineRow {
            cells: cells.into(),
            bg: None,
        })
    }
}

/// Register the mode's theme elements (idempotent by name) and resolve them.
fn resolve_colors(ctx: &ModeContext) -> Colors {
    let Some(theme) = ctx
        .service::<lattice_theme::ThemeRegistryHandle>()
        .map(|outer| (*outer).clone())
    else {
        return FALLBACK;
    };
    let owner = ElementOwner::Mode(OUTPUT_MODE_ID.to_string().into());
    let element = |name: &'static str, palette: &'static str, doc: &'static str| {
        theme.register(
            ElementName::from_static(name),
            owner.clone(),
            StyleSpec::new().fg(ColorRef::Palette(palette.into())),
            doc,
        )
    };
    let running = element(
        "plugin-output.headerline.running",
        "subtext",
        "Plugin output headerline: work in flight.",
    );
    let succeeded = element(
        "plugin-output.headerline.succeeded",
        "green",
        "Plugin output headerline: finished and worked.",
    );
    let failed = element(
        "plugin-output.headerline.failed",
        "red",
        "Plugin output headerline: finished and did not.",
    );
    let text = element(
        "plugin-output.headerline.text",
        "text",
        "Plugin output headerline: the status text.",
    );
    let resolved = theme.resolved();
    let fg = |id: ElementId, fallback: u32| {
        resolved
            .get(id)
            .fg
            .map(|c| c.to_rgb_u32(0))
            .unwrap_or(fallback)
    };
    Colors {
        running: fg(running, FALLBACK.running),
        succeeded: fg(succeeded, FALLBACK.succeeded),
        failed: fg(failed, FALLBACK.failed),
        text: fg(text, FALLBACK.text),
    }
}

/// Replace the whole buffer with `text` as one edit.
async fn replace_all(handle: &Arc<dyn Document>, text: String) {
    let snap = handle.snapshot();
    // ROPE space: the full extent ends past the terminating newline.
    let last = snap.buffer.rope_line_count().saturating_sub(1);
    let last_line = snap.buffer.line(last).unwrap_or_default();
    let end = Position::new(last, last_line.len() as u32);
    if end == Position::ZERO && text.is_empty() {
        return;
    }
    let edit = Edit::replace(Range::new(Position::ZERO, end), text);
    let _ = handle.apply_edit_batch(vec![edit]).await;
}

/// What one drained batch of events does to the buffer, folded so a burst is
/// one write.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Batch {
    /// The buffer is emptied before `text` goes in.
    pub cleared: bool,
    /// Text to append (after the clear, if any).
    pub text: String,
    /// The headerline after the batch: `None` leaves it, `Some(None)` clears
    /// it, `Some(Some(_))` sets it.
    pub status: Option<Option<OutputStatus>>,
}

impl Batch {
    pub(crate) fn is_empty(&self) -> bool {
        !self.cleared && self.text.is_empty() && self.status.is_none()
    }

    pub(crate) fn push(&mut self, step: TailStep) {
        match step {
            TailStep::Append(text) => self.text.push_str(&text),
            TailStep::Clear => {
                // Everything queued so far in this batch was for the page
                // that is being thrown away.
                self.cleared = true;
                self.text.clear();
                self.status = Some(None);
            }
            TailStep::Status(status) => self.status = Some(Some(status)),
        }
    }
}

impl Mode for PluginOutputMode {
    type Guard = Option<Subscription>;

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    /// `read-only-mode` is where the operator gate is; the `ReadOnly` option
    /// below stops Insert-mode typing and nothing else. Declared on the major
    /// because an implied mode is followed from the mode being activated. See
    /// `PluginTraceMode::implies`.
    fn implies(&self) -> &[ModeId] {
        static IMPLIED: std::sync::OnceLock<Vec<ModeId>> = std::sync::OnceLock::new();
        IMPLIED.get_or_init(|| vec![lattice_mode::modes::ReadOnlyMode::mode_id()])
    }

    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, Self::Guard> {
        Box::pin(async move {
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                return Ok(None);
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                return Ok(None);
            };
            let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                return Ok(None);
            };
            let Some(output) = ctx.service::<PluginOutputHandle>() else {
                // No store wired (a harness without plugin support): an empty
                // buffer, never a panic.
                tracing::debug!("plugin-output-mode: no output store; buffer stays empty");
                return Ok(None);
            };
            let output: PluginOutputHandle = (*output).clone();
            let Some(name) = store.name_for(buffer_id) else {
                return Ok(None);
            };
            let wake: Option<InboundBus<OutputLanded>> =
                ctx.service::<OutputWakeHandle>().map(|h| (**h).clone());

            let status = Arc::new(RwLock::new(None::<OutputStatus>));
            let version = Arc::new(AtomicU64::new(1));
            if let Some(registrar) = ctx.service::<Arc<dyn VirtualRowRegistrar>>() {
                let registrar: Arc<dyn VirtualRowRegistrar> = (*registrar).clone();
                let provider = Arc::new(HeaderlineProvider::new(
                    OUTPUT_HEADERLINE_PROVIDER_ID,
                    Arc::new(OutputHeaderline {
                        status: status.clone(),
                        version: version.clone(),
                        colors: resolve_colors(&ctx),
                    }),
                ));
                registrar.unregister(buffer_id, OUTPUT_HEADERLINE_PROVIDER_ID);
                registrar.register(buffer_id, provider as Arc<dyn VirtualRowProvider>);
            }

            // Subscribe FIRST, snapshot second (in the task): nothing can fall
            // between the two, and `Tail` drops what lands in both.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PluginOutputPushed>();
            let sub_id = ctx.events().subscribe_typed::<PluginOutputPushed>(tx);
            let bus_handle = ctx.events_handle();

            runtime.spawn(async move {
                let set_status = |next: Option<OutputStatus>| {
                    if let Ok(mut slot) = status.write() {
                        *slot = next;
                    }
                    version.fetch_add(1, Ordering::Release);
                };
                let landed = || {
                    if let Some(wake) = &wake {
                        let _ = wake.send(OutputLanded);
                    }
                };

                // Seed by REPLACING, so activating again on a buffer that
                // already has text cannot double it.
                let snapshot = output.snapshot(&name).unwrap_or_default();
                let mut tail = Tail::after(&snapshot);
                let mut seed = String::new();
                for line in &snapshot.lines {
                    seed.push_str(line);
                    seed.push('\n');
                }
                replace_all(&handle, seed).await;
                set_status(snapshot.status);
                landed();

                while let Some(first) = rx.recv().await {
                    let mut batch = Batch::default();
                    let mut next = Some(first);
                    while let Some(event) = next {
                        if *event.name == *name {
                            for step in tail.step(event.epoch, event.change) {
                                batch.push(step);
                            }
                        }
                        next = rx.try_recv().ok();
                    }
                    if batch.is_empty() {
                        continue;
                    }
                    if batch.cleared {
                        replace_all(&handle, batch.text).await;
                    } else {
                        append_text(&handle, batch.text).await;
                    }
                    if let Some(next) = batch.status {
                        set_status(next);
                    }
                    landed();
                }
            });

            Ok(Some(Subscription::new(bus_handle, sub_id)))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(state: OutputState, text: &str) -> OutputStatus {
        OutputStatus {
            state,
            text: text.into(),
        }
    }

    fn headerline(initial: Option<OutputStatus>) -> OutputHeaderline {
        OutputHeaderline {
            status: Arc::new(RwLock::new(initial)),
            version: Arc::new(AtomicU64::new(1)),
            colors: FALLBACK,
        }
    }

    fn text_of(row: &HeaderlineRow) -> String {
        row.cells
            .iter()
            .filter_map(|c| char::from_u32(c.codepoint))
            .collect()
    }

    #[test]
    fn no_status_is_no_headerline() {
        assert!(headerline(None).render().is_none());
    }

    #[test]
    fn each_state_has_its_own_icon_and_colour() {
        for (state, icon, fg) in [
            (OutputState::Running, '\u{27f3}', FALLBACK.running),
            (OutputState::Succeeded, '\u{2714}', FALLBACK.succeeded),
            (OutputState::Failed, '\u{2717}', FALLBACK.failed),
        ] {
            let row = headerline(Some(status(state, "work")))
                .render()
                .expect("a status renders");
            assert_eq!(text_of(&row), format!(" {icon} work"));
            assert_eq!(row.cells[1].fg, fg, "{state:?}");
            assert_eq!(row.cells[3].fg, FALLBACK.text);
        }
    }

    #[test]
    fn the_mode_is_a_read_only_no_file_major() {
        let m = PluginOutputMode;
        assert_eq!(m.kind(), ModeKind::Major);
        assert_eq!(m.options().len(), 2, "ReadOnly + NoFile overrides");
        assert_eq!(
            m.implies(),
            &[lattice_mode::modes::ReadOnlyMode::mode_id()],
            "operators are refused only through read-only-mode"
        );
    }

    #[test]
    fn a_batch_folds_appends_into_one_write() {
        let mut batch = Batch::default();
        batch.push(TailStep::Append("a\n".into()));
        batch.push(TailStep::Append("b\n".into()));
        assert_eq!(batch.text, "a\nb\n");
        assert!(!batch.cleared);
        assert_eq!(batch.status, None, "an append leaves the headerline alone");
    }

    #[test]
    fn a_clear_discards_what_the_batch_had_queued_for_the_old_page() {
        let mut batch = Batch::default();
        batch.push(TailStep::Append("old\n".into()));
        batch.push(TailStep::Status(status(OutputState::Failed, "boom")));
        batch.push(TailStep::Clear);
        batch.push(TailStep::Append("new\n".into()));
        assert!(batch.cleared);
        assert_eq!(batch.text, "new\n");
        assert_eq!(
            batch.status,
            Some(None),
            "the old run's status goes with it"
        );
    }

    #[test]
    fn the_last_status_in_a_batch_wins() {
        let mut batch = Batch::default();
        batch.push(TailStep::Status(status(OutputState::Running, "1%")));
        batch.push(TailStep::Status(status(OutputState::Running, "2%")));
        assert_eq!(batch.status, Some(Some(status(OutputState::Running, "2%"))));
        assert!(!batch.is_empty(), "a status alone is still a change");
    }
}
