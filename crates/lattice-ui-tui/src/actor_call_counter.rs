//! Per-thread counter for `App::read_editor` / `App::mutate_editor`
//! / `App::mutate_editor_with` calls.
//!
//! Slice `3c.extension.fold-rs.test`: regression-test scaffolding
//! that asserts the per-frame paint path doesn't reach the actor
//! mailbox. The previous regression — `frame_120_lines/200` went
//! from 90µs to 43.73ms because 120 per-line `read_editor` calls
//! crept into `compose_visible_lines_inner` — would have been
//! caught by a "this path makes 0 actor calls" assertion if one
//! had existed.
//!
//! The counter is a `Cell<u64>` in thread-local storage; the
//! seam methods `bump()` on entry. Tests call [`snapshot`] to
//! read the current count, drive the path under test, then call
//! [`snapshot`] again and assert the delta is bounded.
//!
//! Non-test builds: one inlined thread-local increment per seam
//! call (~ns). Negligible against the ~µs–~100µs cost the seam
//! itself pays.

use std::cell::Cell;

thread_local! {
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

/// Increment the per-thread call count. Called by the
/// `App::{read,mutate,mutate_with}_editor` seam.
#[inline]
pub fn bump() {
    COUNT.with(|c| c.set(c.get() + 1));
}

/// Read the current count. Tests use this to capture before/after
/// snapshots around a code path under test.
#[inline]
pub fn snapshot() -> u64 {
    COUNT.with(|c| c.get())
}

/// Reset the count to zero. Optional — tests can also just
/// compute deltas via `snapshot()` before / after.
#[inline]
pub fn reset() {
    COUNT.with(|c| c.set(0));
}
