//! `messages-mode` -- major mode for the editor's `*messages*`
//! audit-log buffer (design.md §5.10.6).
//!
//! Single buffer in the editor (`*messages*`). The mode's job
//! is small but architecturally important:
//!
//! - **Identity:** the buffer's major mode IS `messages-mode`,
//!   not `text-mode + read-only-mode`. Symmetric with
//!   `lsp-log-mode` for `*lsp*`. The renderer can branch on
//!   `messages-mode` if it ever needs to (today it doesn't).
//! - **Read-only contribution:** the mode contributes
//!   `ReadOnly = true` so the modal dispatcher gates user
//!   keystrokes; subsystem writes bypass via
//!   `apply_edit_batch_blocking`.
//! - **Future home for the tracing-subscriber lifecycle**
//!   (deferred to v1.1). msg-mode.1 installs the global
//!   subscriber once at App boot; making the subscriber's
//!   enable/disable mode-driven is a refinement that lives in
//!   the mode's `Guard` once it lands.
//!
//! Marker mode for v1: `type Guard = ();`, trivial
//! `on_activate`. The work the spec attributes to
//! "registers a `tracing::Subscriber` at activate time" is
//! split across the App boot path
//! (`lattice_runtime::install_messages_subscriber`) for v1
//! simplicity. The mode-driven lifecycle binding lands when
//! reload-based subscriber control is wired in v1.1.

use crate::{CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind};
use lattice_config::OptionOverrideSet;

/// Major mode for the `*messages*` buffer.
pub struct MessagesMode;

impl MessagesMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("messages-mode")
    }
}

impl Mode for MessagesMode {
    type Guard = ();
    fn id(&self) -> ModeId {
        Self::mode_id()
    }
    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }
    fn options(&self) -> OptionOverrideSet {
        // User keystrokes can't mutate `*messages*` -- the
        // subsystem owns the content. Subsystem writes route
        // through `apply_edit_batch_blocking` which bypasses
        // the dispatcher's read-only gate.
        //
        // `NoFile = true`: `*messages*` is a transcript, not an
        // on-disk file. `:q` must not warn about unsaved
        // changes; `:w` is a no-op.
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::NoFile = true,
        }
    }
    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }
    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_kind() {
        assert_eq!(MessagesMode.id(), MessagesMode::mode_id());
        assert_eq!(MessagesMode::mode_id().as_str(), "messages-mode");
        assert_eq!(MessagesMode.kind(), ModeKind::Major);
    }

    #[test]
    fn contributes_read_only_and_no_file() {
        let opts = <MessagesMode as Mode>::options(&MessagesMode);
        assert_eq!(
            opts.iter().count(),
            2,
            "expected ReadOnly + NoFile contributions",
        );
    }
}
