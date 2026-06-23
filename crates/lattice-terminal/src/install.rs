//! BC.4: the crate-owned `install(boot)` entry point.
//!
//! Terminal's only host-side boot *wiring* was its mode registration; this
//! moves it into one Phase-B line (`lattice_terminal::install(&mut boot)`)
//! against the generic [`SubsystemBoot`] surface, matching the claude-code
//! shape (BC.3b).
//!
//! Two terminal touch-points intentionally stay host-side, and are *not*
//! mode-ownership violations:
//!
//! - The `TerminalStoreHandle` service is a **host-published primitive** — the
//!   host's `BufferRegistry` exposed under `dyn TerminalStore` (the
//!   `impl TerminalStore for BufferRegistry` lives in `lattice-host`). It's the
//!   same category as `buffer_store` / `diagnostics`: the host owns the data,
//!   the terminal *mode* consumes it via `services.get`. The `SubsystemBoot`
//!   surface can't carry a terminal-specific type, so it stays host-registered
//!   until/unless terminal impls `TerminalStore` over `BufferStoreHandle`.
//! - The `Editor`-coupled invocation runner (`Editor::run_terminal_invocation`)
//!   is the shared invocation-runner mechanism (Help / Oil / FileTree / Terminal
//!   all bind `Editor::run_*_invocation`); migrating it is a cross-cutting
//!   cleanup, not a terminal slice.

use lattice_mode::SubsystemBoot;

use crate::modes::register_terminal_modes;

/// Wire the terminal subsystem into the editor at boot.
pub fn install(boot: &mut impl SubsystemBoot) {
    // Issue #40 / Terminal-mode T1: register `terminal-mode` (+ Normal / Insert)
    // so option contributions (ReadOnly + NoFile) apply to Terminal buffers and
    // each mode's `on_activate` installs its own keymap / handlers.
    register_terminal_modes(boot.modes_mut());
}
