//! `dashboard-mode` — the major mode for the `*dashboard*` launch buffer
//! (DB.2). It is self-contained: it contributes the read-only / gutterless
//! option set directly (the same set help-mode contributes), so the dashboard
//! needs no help-mode dependency for behaviour. `<CR>`-follow and Esc reuse
//! the help machinery via the `BufferKind::Dashboard` gates
//! (`dashboard.md` §9.2), not via a mode.
//!
//! Later slices hang more off this mode: the `dashboard.*` theme elements
//! (DB.3) and the branding virtual-row provider (DB.4) register in
//! `on_activate`.

use lattice_config::OptionOverrideSet;
use lattice_core::BufferKind;
use lattice_mode::{LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, ModeRegistry};
use lattice_theme::{ElementOwner, ThemeRegistryHandle};

use crate::theme::register_dashboard_theme_elements;

pub struct DashboardMode;

impl DashboardMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("dashboard-mode")
    }
}

impl Mode for DashboardMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    /// Bind this major to `BufferKind::Dashboard` so `find_major_for_kind`
    /// routes the `*dashboard*` buffer to it.
    fn target_buffer_kind(&self) -> Option<BufferKind> {
        Some(BufferKind::Dashboard)
    }

    /// Read-only, wrapped, no-file, gutterless — the same contribution set
    /// help-mode carries. `ReadOnly` + `NoFile` mean the dispatcher gates
    /// keystrokes and `:q` skips the dirty check; `Number = false` +
    /// `signcolumn = no` render the page gutterless.
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
            lattice_config::Wrap = true,
            lattice_config::NoFile = true,
            lattice_config::Number = false,
            lattice_config::SignColumnOption = lattice_config::SignColumn::No,
        }
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async move {
            // DB.3: the mode owns its `dashboard.*` theme element vocabulary
            // — register it here (idempotent by name). A missing service
            // (test harness) just skips; the branding provider then renders
            // with fallback colours.
            if let Some(theme) = ctx
                .service::<ThemeRegistryHandle>()
                .map(|outer| (*outer).clone())
            {
                let owner = ElementOwner::Mode(Self::mode_id().as_str().to_string().into());
                let _ = register_dashboard_theme_elements(theme.as_ref(), owner);
            }
            Ok(())
        })
    }
}

/// Register `dashboard-mode` into the mode registry. Called from `install`
/// via `boot.modes_mut()`.
pub fn register_dashboard_modes(registry: &mut ModeRegistry) {
    registry
        .register(DashboardMode)
        .expect("dashboard-mode register");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_kind_and_target() {
        assert_eq!(DashboardMode.id(), DashboardMode::mode_id());
        assert_eq!(DashboardMode::mode_id().as_str(), "dashboard-mode");
        assert_eq!(DashboardMode.kind(), ModeKind::Major);
        assert_eq!(
            DashboardMode.target_buffer_kind(),
            Some(BufferKind::Dashboard)
        );
    }

    #[test]
    fn contributes_read_only_gutterless_set() {
        // ReadOnly + Wrap + NoFile + Number + SignColumn.
        assert_eq!(DashboardMode.options().iter().count(), 5);
    }
}
