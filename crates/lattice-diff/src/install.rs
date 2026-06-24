//! BC.6 / DX.7: the crate-owned `install(boot)` entry point.
//!
//! The host-side diff *wiring* that can move against the generic
//! [`SubsystemBoot`] surface is the mode registration; this collapses it into
//! one Phase-B line (`lattice_diff::install(&mut boot)`), matching the
//! terminal / claude-code shape (BC.3b/BC.4) and closing BC.6.
//!
//! Three diff touch-points intentionally stay host-side, and are *not*
//! mode-ownership violations:
//!
//! - **The `diff-mode` keymap layer** is pushed host-side from the live
//!   `KeymapHandle` (the host owns it and reads the `CommandRegistry` for name
//!   resolution), calling the mode-owned [`crate::mode::diff_mode_layer_bindings`]
//!   — the emacs-keys pattern (BC.5: "the host retains only the keymap-layer
//!   push"). The binding choice, the name-based builder, AND the `do_diff_*`
//!   handler bodies all live in this crate / its effect contract; the host
//!   performs only the mechanical push. `SubsystemBoot` exposes no
//!   keymap-push primitive by design.
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
