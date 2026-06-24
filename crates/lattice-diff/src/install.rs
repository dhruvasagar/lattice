//! BC.6 / DX.7: the crate-owned `install(boot)` entry point.
//!
//! The host-side diff *wiring* that can move against the generic
//! [`SubsystemBoot`] surface is the mode registration; this collapses it into
//! one Phase-B line (`lattice_diff::install(&mut boot)`), matching the
//! terminal / claude-code shape (BC.3b/BC.4) and closing BC.6.
//!
//! The diff-mode `do`/`dp` keymap is **fully mode-owned** (MO.x, 2026-06-24):
//! contributed through [`crate::mode::DiffMode::keymap`] and pushed by the
//! host's generic K.2.4 `translate_mode_keymaps` pass (under
//! `MinorMode(diff-mode)`, K.1.c-gated, `cmd` names resolved against the
//! `CommandRegistry`) — exactly like every other mode's keymap. No
//! diff-specific host push remains.
//!
//! Two diff touch-points intentionally stay host-side, and are *not*
//! mode-ownership violations:
//!
//! - **The `DiffSubsystem` lifecycle** — `bind` with the host's
//!   `BufferRegistryDocumentResolver`, the `diff_subsystem` /
//!   `diff_subscription_guard` / `diff_forwarders` Editor fields, and the
//!   `apply_pending_diff_mode_changes` dispatch-tail drain — is host
//!   actor-loop state, the same category as terminal's invocation runner. The
//!   subsystem reads only the `BufferTextProvider` / `DocumentBufferResolver`
//!   seams (C6), which the host supplies; the `SubsystemBoot` surface can't
//!   carry the host `BufferRegistry`-backed resolver, so the bind stays
//!   host-side. The subsystem handle is still published as a service
//!   (`DiffSubsystemHandle`) so the mode's `on_activate` reaches it generically.
//! - **The `+N ~M` modeline element** is registered against the host's
//!   `ModelineService`, which is created *after* the Phase-B install list runs
//!   (boot ordering), so its registration stays in the host's modeline-setup
//!   block. The descriptor + the `diff_content` formatter are mode-owned (in
//!   [`crate::mode`]); only the registration *call* is host-sequenced.
//!
//! See `docs/dev/architecture/diff-extraction.md` (couplings C6/C10) for the
//! cut and the rationale.

use lattice_mode::SubsystemBoot;

use crate::mode::register_diff_modes;

/// Wire the diff subsystem's modes into the editor at boot.
pub fn install(boot: &mut impl SubsystemBoot) {
    // D.5.a: register `diff-mode` (the marker minor mode K.1.c gates the
    // `do`/`dp` chords on; its `on_activate` registers the buffer's
    // hunk-fold source, DX.3-C7). DX.8 adds `diff-conflict-mode` here.
    register_diff_modes(boot.modes_mut());
}
