//! Which-key — the pending-chord discoverability subsystem.
//!
//! Design: `docs/dev/architecture/which-key.md` (§3 ownership, §5
//! lifecycle, §8 options). Slice plan:
//! `docs/dev/operations/slice-plans/which-key.md` (WK.6).
//!
//! Hold a prefix; after a short idle delay a popup shows what can come
//! next, derived from the live composite keymap the dispatcher itself
//! walks — never from the static catalog (design §2, and the bug
//! `:describe-bindings` still has).
//!
//! ## Shape
//!
//! Four moving parts, all owned here:
//!
//! 1. [`install`] wires the subsystem against the generic
//!    [`SubsystemBoot`](crate::SubsystemBoot) surface. It adds ZERO
//!    `Editor::` methods and ZERO host `Action` variants — the
//!    mode-ownership acid test.
//! 2. A `PartialChordPending` subscription stashes the payload and arms
//!    an idle gate.
//! 3. The gate's handler builds the model + grid and emits
//!    `Effect::OpenPopup`.
//! 4. [`WhichKeyMode`] is the popup buffer's major mode; its
//!    `on_activate` writes the stashed grid into the buffer it was
//!    activated on.
//!
//! ## The popup is passive
//!
//! `PopupFocus::Passive` — the document keeps focus, the caret and the
//! modal state, so **every keystroke continues to flow to the trie
//! unchanged**. A hint that changed what a chord does would be a vim
//! deviation nobody asked for, and one that failed differently for every
//! prefix (a transient-style takeover's `<C-n>` shadows a real `n`
//! continuation under `<C-w>`, and a real `j` under `g`) is the worst
//! shape of that failure. See design §7.

use std::sync::{Arc, Mutex};

use lattice_config::ConfigRegistry;
use lattice_core::ui::popup::{PopupFocus, PopupPlacement};
use lattice_grammar::CommandRegistryHandle;
use lattice_grammar::effect::Effect;
use lattice_keymap::PartialChordPending;
use lattice_keymap::which_key::{GridOpts, Sort, layout_grid};

use crate::{
    BufferStoreHandle, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    SubsystemBoot,
};

/// The popup buffer's registered name.
pub const WHICH_KEY_BUFFER_NAME: &str = "*which-key*";

lattice_config::groups! {
    /// Pending-chord discoverability.
    pub WhichKey = "which-key";
}

lattice_config::options! {
    group = WhichKey;

    /// Show the pending-chord popup at all.
    #[name("which-key.enabled")]
    pub WhichKeyEnabled: bool = true;

    /// Milliseconds a prefix must sit pending before the popup appears.
    /// `0` shows it immediately. The delay is what separates a hint from
    /// a stutter: a user who knows their chord finishes it well inside
    /// the window and never sees a frame of popup.
    #[name("which-key.delay")]
    pub WhichKeyDelay: i64 = 300;

    /// Maximum content rows. Hard-capped at half the pane regardless.
    #[name("which-key.max-height")]
    pub WhichKeyMaxHeight: i64 = 12;

    /// Maximum grid columns.
    #[name("which-key.max-columns")]
    pub WhichKeyMaxColumns: i64 = 6;

    /// Row ordering: `key` (digits, lowercase, uppercase, punctuation,
    /// special, modifier-bearing) or `label`.
    #[name("which-key.sort")]
    pub WhichKeySort: String = String::from("key");
}

/// What the subscription stashes for the gate handler to read. Carrying
/// the whole event is deliberate — see `PartialChordPending`'s docs for
/// why the payload rides rather than being read back.
type Stash = Arc<Mutex<Option<PartialChordPending>>>;

/// The grid the gate handler laid out, for [`WhichKeyMode::on_activate`]
/// to write into the popup buffer. A mode cannot create the buffer it is
/// activating on, and the handler cannot write a buffer that does not
/// exist yet, so the content crosses between them here.
type PendingGrid = Arc<Mutex<Vec<String>>>;

/// Major mode for `*which-key*`.
pub struct WhichKeyMode {
    grid: PendingGrid,
}

impl WhichKeyMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("which-key-mode")
    }
}

impl Mode for WhichKeyMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    /// Read-only and file-less. `ReadOnly` alone gates typing only, so
    /// `read-only-mode` is implied for the operator gate — declared on
    /// the MAJOR, since an implied mode is followed from the mode being
    /// activated.
    fn implies(&self) -> &[ModeId] {
        static IMPLIED: std::sync::OnceLock<Vec<ModeId>> = std::sync::OnceLock::new();
        IMPLIED.get_or_init(|| vec![crate::modes::ReadOnlyMode::mode_id()])
    }

    fn options(&self) -> lattice_config::OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
            // A hint with a gutter of line numbers reads as a document.
            lattice_config::Number = false,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        let grid = Arc::clone(&self.grid);
        Box::pin(async move {
            let text = {
                let g = grid.lock().unwrap_or_else(|e| e.into_inner());
                g.join("\n")
            };
            let buffer_id = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(store) = ctx.service::<BufferStoreHandle>() else {
                // No buffer store wired (a test harness); the popup opens
                // empty rather than panicking. Log-and-skip, at debug —
                // this is keystroke-adjacent.
                tracing::debug!("which-key: no buffer store; popup left empty");
                return Ok(());
            };
            let Some(handle) = store.handle_for(buffer_id) else {
                tracing::debug!(?buffer_id, "which-key: popup buffer vanished");
                return Ok(());
            };
            let snap = handle.snapshot();
            let last_line = snap.buffer.rope_line_count().saturating_sub(1);
            let last_len = snap.buffer.line(last_line).unwrap_or_default().len() as u32;
            let range = lattice_protocol::Range::new(
                lattice_protocol::position::Position::new(0, 0),
                lattice_protocol::position::Position::new(last_line, last_len),
            );
            let _ = handle
                .apply_edit_batch(vec![lattice_protocol::edit::Edit::replace(range, text)])
                .await;
            Ok(())
        })
    }
}

/// Register `which-key-mode`. Phase B, in the host's install list.
///
/// ## Why this is two calls and not one
///
/// The design's acid test wants a subsystem to touch the host in exactly
/// one place. Which-key cannot, and the reason is boot ordering rather
/// than design: the MODE registry freezes early (`freeze_mode_registry`,
/// before commands are all registered), while the COMMAND registry
/// handle this subsystem needs at popup-build time — rungs 2 and 3 of
/// the label chain — only exists after `freeze_command_registry`, much
/// later. A single install call would have to sit on one side of that
/// gap or the other: register the mode and get `None` for the command
/// service (every label degrading to `<unbound>`, silently), or resolve
/// the services and panic registering a mode into a frozen registry.
///
/// So: [`install`] registers the mode, [`wire`] wires the lifecycle once
/// the handles exist. Two lines in `editor_boot`, no `Editor::` method
/// and no host `Action` variant — the part of the acid test that is
/// actually about ownership still holds.
pub fn install(boot: &mut impl SubsystemBoot) -> WhichKeyGrid {
    let grid: PendingGrid = Arc::new(Mutex::new(Vec::new()));
    // A duplicate registration is a boot-order bug, not a runtime
    // condition — log it and carry on rather than unwrapping.
    if let Err(e) = boot.modes_mut().register(WhichKeyMode {
        grid: Arc::clone(&grid),
    }) {
        tracing::debug!(error = %e, "which-key: mode already registered");
    }
    WhichKeyGrid(grid)
}

/// The grid cell [`install`] created, handed to [`wire`] so both halves
/// write and read the same one. Opaque: the host only carries it between
/// the two calls.
pub struct WhichKeyGrid(PendingGrid);

/// Wire which-key's lifecycle: the idle gate, the `PartialChordPending`
/// subscription, and the dismissal path. Called after the keymap and
/// command-registry services are registered — see [`install`] for why
/// that cannot be the same call.
pub fn wire(boot: &mut impl SubsystemBoot, grid: WhichKeyGrid) {
    let grid = grid.0;
    let stash: Stash = Arc::new(Mutex::new(None));

    let config = boot.service::<Arc<ConfigRegistry>>();
    let keymap = boot.service::<lattice_keymap::KeymapHandle>();
    let commands = boot.service::<CommandRegistryHandle>();

    // The gate's body: the delay elapsed with a prefix still pending.
    let gate = Arc::new(boot.idle_gate(
        "which-key",
        Box::new({
            let stash = Arc::clone(&stash);
            let grid = Arc::clone(&grid);
            let config = config.clone();
            move || {
                let Some(pending) = stash
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .filter(|p| !p.chords.is_empty())
                else {
                    // The prefix evaporated during the delay (the chord
                    // resolved, a mode deactivated, `:map` rebuilt the trie).
                    // No popup, no complaint.
                    return Vec::new();
                };
                let (Some(keymap), Some(commands)) = (keymap.as_ref(), commands.as_ref()) else {
                    tracing::debug!("which-key: keymap/command service missing; no popup");
                    return Vec::new();
                };
                let Some(node) = keymap.continuations_with_context(
                    pending.binding_mode,
                    &pending.chords,
                    &pending.active_modes,
                ) else {
                    return Vec::new();
                };
                if node.is_empty() {
                    // Bound with nothing beneath it: an empty box would be
                    // worse than no box.
                    return Vec::new();
                }
                let opts = read_config(config.as_deref().map(|c| &**c));
                let model = lattice_keymap::which_key::build_model(
                    node,
                    &pending.chords,
                    pending.binding_mode,
                    &commands.load(),
                    opts.sort,
                );
                let lines = layout_grid(
                    &model,
                    pending.pane_width as usize,
                    GridOpts {
                        max_columns: opts.max_columns,
                        max_height: opts.max_height,
                    },
                );
                if lines.is_empty() {
                    // Pane too narrow (§8): a single column of truncated
                    // labels is worse than nothing.
                    return Vec::new();
                }
                *grid.lock().unwrap_or_else(|e| e.into_inner()) = lines;
                vec![Effect::OpenPopup {
                    name: WHICH_KEY_BUFFER_NAME.to_string(),
                    mode_id: WhichKeyMode::mode_id().as_str().to_string(),
                    placement: PopupPlacement::PaneBottom,
                    // State A: the document keeps focus and every keystroke
                    // still resolves against the trie. See the module docs.
                    focus: PopupFocus::Passive,
                }]
            }
        }),
    ));

    // The arming path. `inbound`'s send wakes the editor, and its handler
    // runs on the actor thread — so arming happens where the gate lives,
    // and a fired gate repaints without a keystroke.
    let inbound = boot.inbound({
        let stash = Arc::clone(&stash);
        let gate = Arc::clone(&gate);
        let config = config.clone();
        let mut popup_open = false;
        move |ev: PartialChordPending| {
            let enabled = config
                .as_ref()
                .and_then(|c| c.get_typed::<WhichKeyEnabled>())
                .map(|v| *v)
                .unwrap_or(true);
            if ev.chords.is_empty() || !enabled {
                gate.disarm();
                *stash.lock().unwrap_or_else(|e| e.into_inner()) = None;
                if std::mem::take(&mut popup_open) {
                    return vec![Effect::DismissPopup];
                }
                return Vec::new();
            }
            let delay = config
                .as_ref()
                .and_then(|c| c.get_typed::<WhichKeyDelay>())
                .map(|v| *v)
                .unwrap_or(300)
                .max(0) as u64;
            *stash.lock().unwrap_or_else(|e| e.into_inner()) = Some(ev);
            gate.arm(tokio::time::Instant::now() + std::time::Duration::from_millis(delay));
            popup_open = true;
            Vec::new()
        }
    });

    // Bus → inbound. Subscribed synchronously (before the spawn) so no
    // early event is lost to a task that has not been polled yet — the
    // same ordering `lattice_dashboard::install_startup_trigger` uses.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<PartialChordPending>();
    boot.event_bus().subscribe_typed(tx);
    boot.runtime_handle().spawn(async move {
        while let Some(ev) = rx.recv().await {
            if inbound.send(ev).is_err() {
                break;
            }
        }
    });
}

/// Resolved option values for one popup build.
struct ResolvedOpts {
    max_columns: usize,
    max_height: usize,
    sort: Sort,
}

fn read_config(config: Option<&ConfigRegistry>) -> ResolvedOpts {
    let int = |c: &ConfigRegistry, d: i64, f: fn(&ConfigRegistry) -> Option<i64>| f(c).unwrap_or(d);
    let (max_columns, max_height, sort) = match config {
        Some(c) => (
            int(c, 6, |c| c.get_typed::<WhichKeyMaxColumns>().map(|v| *v)).max(1) as usize,
            int(c, 12, |c| c.get_typed::<WhichKeyMaxHeight>().map(|v| *v)).max(1) as usize,
            c.get_typed::<WhichKeySort>()
                .and_then(|v| {
                    let parsed = Sort::parse(v.as_str());
                    if parsed.is_none() {
                        // Unknown value: fall back rather than fail, and
                        // say so once at debug (§8 — this is
                        // keystroke-adjacent, so never `info!`).
                        tracing::debug!(value = %*v, "which-key.sort: unknown value; using `key`");
                    }
                    parsed
                })
                .unwrap_or_default(),
        ),
        None => (6, 12, Sort::default()),
    };
    ResolvedOpts {
        max_columns,
        max_height,
        sort,
    }
}

/// `*which-key*` is a popup buffer, so it never wants a `BufferKind` of
/// its own — the popup machinery stores it as `BufferData::Help`. This
/// asserts the mode does not claim a kind, which would route ordinary
/// buffers to it.
#[cfg(test)]
mod tests {
    use super::*;

    fn mode() -> WhichKeyMode {
        WhichKeyMode {
            grid: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[test]
    fn id_and_kind() {
        assert_eq!(mode().id().as_str(), "which-key-mode");
        assert_eq!(mode().kind(), ModeKind::Major);
        assert_eq!(
            <WhichKeyMode as Mode>::target_buffer_kind(&mode()),
            None::<lattice_core::BufferKind>,
            "the popup buffer is reached by name, not by kind"
        );
    }

    /// Read-only takes TWO declarations: the option gates typing, and
    /// `read-only-mode` carries the invocation runner that refuses
    /// operators. A hint you can `dd` into is not read-only.
    #[test]
    fn read_only_is_declared_twice() {
        let m = mode();
        assert!(
            <WhichKeyMode as Mode>::implies(&m).contains(&crate::modes::ReadOnlyMode::mode_id()),
            "the option alone gates Insert-mode typing and nothing else"
        );
        let opts = <WhichKeyMode as Mode>::options(&m);
        assert_eq!(opts.iter().count(), 3, "ReadOnly + NoFile + Number");
    }

    #[test]
    fn sort_option_parses_and_falls_back() {
        assert_eq!(Sort::parse("key"), Some(Sort::Key));
        assert_eq!(Sort::parse("label"), Some(Sort::Label));
        assert_eq!(Sort::parse("sideways"), None, "unknown → caller defaults");
    }

    #[test]
    fn defaults_are_read_when_no_config_is_wired() {
        let o = read_config(None);
        assert_eq!(o.max_columns, 6);
        assert_eq!(o.max_height, 12);
        assert_eq!(o.sort, Sort::Key);
    }
}
