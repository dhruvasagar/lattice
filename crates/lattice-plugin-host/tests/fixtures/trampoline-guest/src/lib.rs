//! PH7.3d trampoline fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `trampoline-fixture`
//! world. It exists to exercise, through a real guest↔host canonical-ABI call:
//!   - §4.1 `apply-effect`: takes a projected context (`args`) and returns
//!     `list<effect>` — the operator/motion `apply` shape. It echoes a
//!     `String` arg back as an `Effect::Echo` (proving data flows in), else
//!     returns a fixed two-effect list (a unit arm + a string-payload arm) so
//!     the host can assert the exact round-trip of the `effect` mirror.
//!   - §4.3 `next-batch`: returns one batch per call, an empty list when
//!     exhausted, so the host can drive the result-carrier loop it owns.

wit_bindgen::generate!({
    world: "trampoline-fixture",
    path: "../../../../../wit",
});

use std::sync::atomic::{AtomicU32, Ordering};

// World-`use`d types (`args`, `effect`) surface at the crate root; the
// transitively-referenced payload records live under the generated `types`
// module — same split as the host side.
use crate::lattice::plugin_host::types::{EchoLevel, EchoPayload};

struct Component;

/// Batch cursor for `next-batch`. Component instances are single-threaded, but
/// an atomic keeps it safe with zero `unsafe`.
static BATCH_CURSOR: AtomicU32 = AtomicU32::new(0);

impl Guest for Component {
    fn apply_effect(a: Args) -> Vec<Effect> {
        match a {
            // Data flows in: echo a string arg back out as an effect.
            Args::String(s) => vec![Effect::Echo(EchoPayload {
                level: EchoLevel::Info,
                text: s,
            })],
            // Otherwise a fixed list exercising a unit arm + a string-payload
            // arm — the host asserts this exact sequence round-trips.
            _ => vec![
                Effect::RecordJump,
                Effect::SetColorscheme("nord".to_string()),
            ],
        }
    }

    fn next_batch() -> Vec<String> {
        // Three logical batches: ["a","b"], ["c"], [] (exhausted).
        match BATCH_CURSOR.fetch_add(1, Ordering::Relaxed) {
            0 => vec!["a".to_string(), "b".to_string()],
            1 => vec!["c".to_string()],
            _ => Vec::new(),
        }
    }
}

export!(Component);
