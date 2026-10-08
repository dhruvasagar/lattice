//! `oil-global-mode` -- the minor mode that owns `-`, everywhere.
//!
//! `-` means "show me the directory this is in", and it is meant to work
//! from any buffer: a file opens oil on its directory, an oil listing
//! steps to its parent, a file-tree row opens oil on the row's directory.
//! That reach is why the chord used to sit in the host's Builtin keymap
//! with its body on `Editor` — the one layer that fires in every buffer.
//! But Builtin is universal vim grammar, and this is a feature of the
//! listing crate. A [`Universal`](ActivationPolicy::Universal) minor mode
//! has the same reach and lives with the feature.
//!
//! ## Why oil-mode and file-tree-mode do not bind `-` themselves
//!
//! A minor mode's keymap outranks a major's, and this minor is active in
//! oil and file-tree buffers too. A `-` on either major would be
//! unreachable. So the chord is declared once, here, and the handler asks
//! each listing for its own answer first: [`oil_parent_directory`] and
//! [`file_tree_row_directory`] stay in the modules that own the state they
//! read. It branches on which buffer-local a buffer carries, not on its
//! kind.
//!
//! ## History
//!
//! Before this mode, oil-mode declared its `-` under the host's command
//! name, `action:oil-navigate-up`. Handlers bind by name, so oil-mode's
//! handler captured the file-buffer chord, declined there, and `-` did
//! nothing in a file buffer for a release. One owner for the chord is the
//! structural fix for that.

use std::sync::{Arc, OnceLock};

use lattice_grammar::Effect;
use lattice_mode::{
    ActionContext, ActionHandler, ActionHandlerContribution, ActivationPolicy, CapabilitySet,
    Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry,
    keymap_entry,
};

use super::modes::{OilDir, oil_parent_directory};
use crate::file_tree::modes::{FileTreeEntries, file_tree_row_directory};

/// The command `-` resolves to. Declared in [`crate::install`].
pub const NAVIGATE_UP: &str = "action:oil-navigate-up";

/// Minor mode owning `-`. A marker mode: a keymap, a handler and an
/// activation policy, no per-buffer resources.
pub struct OilGlobalMode;

impl OilGlobalMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("oil-global-mode")
    }
}

fn oil_global_mode_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![keymap_entry!(
            mode: Normal,
            chord: "-",
            doc: "Open oil on the directory containing this buffer's file; in a listing, go up to the parent directory.",
            cmd: "action:oil-navigate-up"
        )]
    })
}

/// What `-` does in the buffer `ctx` describes.
fn navigate_up(ctx: &ActionContext<'_>) -> Option<Effect> {
    if ctx.buffer_local::<OilDir>().is_some() {
        return oil_parent_directory(ctx);
    }
    if ctx.buffer_local::<FileTreeEntries>().is_some() {
        return file_tree_row_directory(ctx);
    }
    // Anything else: no directory given means "the current file's", and
    // the host's applier lands the cursor on that file.
    Some(Effect::OpenOil { dir: None })
}

impl Mode for OilGlobalMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    /// `Universal`, not `Global`: the chord it replaces was a Builtin
    /// binding, live in help, the dashboard, terminals and the listings
    /// themselves, not only in document buffers.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Universal
    }
    fn keymap(&self) -> Keymap {
        Keymap::from_entries(oil_global_mode_keymap_entries())
    }
    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        let handler: ActionHandler = Arc::new(navigate_up);
        vec![ActionHandlerContribution {
            action_name: NAVIGATE_UP,
            handler,
        }]
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// Register `oil-global-mode` against `registry`.
pub fn register_oil_global_mode(registry: &mut ModeRegistry) {
    registry
        .register(OilGlobalMode)
        .expect("oil-global-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oil_global_mode_is_a_universal_minor() {
        assert_eq!(OilGlobalMode::mode_id().as_str(), "oil-global-mode");
        assert_eq!(OilGlobalMode.kind(), ModeKind::Minor);
        assert!(matches!(
            OilGlobalMode.activation_policy(),
            ActivationPolicy::Universal
        ));
    }

    #[test]
    fn oil_global_mode_binds_dash_to_the_command_its_handler_answers() {
        let km = OilGlobalMode.keymap();
        let bound: Vec<(&str, Option<&str>)> =
            km.entries.iter().map(|e| (e.chord, e.command)).collect();
        assert_eq!(bound, vec![("-", Some(NAVIGATE_UP))]);
        let handled: Vec<&str> = OilGlobalMode
            .action_handlers()
            .iter()
            .map(|c| c.action_name)
            .collect();
        assert_eq!(handled, vec![NAVIGATE_UP]);
    }

    /// The majors must not bind `-`: this minor outranks them, so a `-`
    /// declared there could never fire — and nothing would say so.
    #[test]
    fn the_listing_majors_leave_dash_to_this_mode() {
        for (name, km) in [
            ("oil-mode", crate::oil::OilMode.keymap()),
            ("file-tree-mode", crate::file_tree::FileTreeMode.keymap()),
        ] {
            assert!(
                km.entries.iter().all(|e| e.chord != "-"),
                "{name} binds `-`, which oil-global-mode shadows",
            );
        }
    }
}
