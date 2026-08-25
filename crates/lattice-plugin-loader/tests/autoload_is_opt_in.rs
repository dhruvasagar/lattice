//! Plugin auto-discovery is opt-in, and both halves of that need pinning.
//!
//! ## What this guards, and why it is worth a test
//!
//! It used to be opt-*out*, and 42 of the 45 test files that boot a real
//! `Editor` never opted out. Each one silently loaded the developer's real
//! `~/.config/lattice/plugins/`, so what a test did depended on what the
//! person running it had installed — CI with an empty home and a laptop with
//! an org plugin disagreed, and nothing in the failure said so.
//!
//! The concrete casualty was `lattice-host`'s `lsp_async_wake.rs`, which
//! asserts no `async_landed` wake fires within a second of settling. A real
//! plugin load fires one. It passed for everyone until a plugin was installed,
//! then failed on that machine only.
//!
//! A per-file opt-out is a thing the 43rd file forgets and whose absence does
//! not announce itself, so the default is sealed instead.
//!
//! ## One test, deliberately
//!
//! The latch is process-global. Split across two `#[test]`s these assertions
//! would race — cargo runs them on a thread pool, so "assert it starts sealed"
//! and "enable it" would interleave and the suite would flake in exactly the
//! way this file exists to stop. Sequencing them inside one test is what makes
//! the ordering a property of the code rather than of the scheduler.

#![allow(clippy::unwrap_used)]

use lattice_plugin_loader::{autoload_enabled, disable_autoload, enable_autoload};

#[test]
fn autoload_is_off_until_a_binary_asks_for_it() {
    // The property the whole inversion exists for: a process that says
    // nothing gets no auto-discovery. This is the assertion that fails if the
    // default is ever flipped back.
    assert!(
        !autoload_enabled(),
        "auto-discovery must be OFF until something opts in — this is what \
         stops a test picking up the developer's real ~/.config/lattice"
    );

    // The other half. Inverting the default traded a silent, machine-dependent
    // failure for a loud one: if the binary's `enable_autoload()` call is ever
    // lost, a shipped editor loads no plugins. Pinning the lever means that
    // loss would be a deleted call rather than a broken mechanism.
    enable_autoload();
    assert!(
        autoload_enabled(),
        "the binary's opt-in must actually turn discovery on"
    );

    // And a process that enabled discovery has to be able to seal it again —
    // the honest inverse, and what an embedder needs.
    disable_autoload();
    assert!(!autoload_enabled(), "the inverse must still seal it");
}
