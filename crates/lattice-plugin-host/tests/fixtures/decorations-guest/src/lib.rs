//! PH7.9c decoration fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `decorations-plugin`
//! world, driving the decoration producer actor (`decoration_task.rs`) through a
//! real host→guest `gutter-decorations` call:
//!   - the producer returns a deterministic set derived from the projected
//!     `decoration-context`: a `Diff{Change}` on line 0, a `Severity{Error}` on
//!     line 1, and a `Diff{Add}` on the LAST line (`line_count - 1`) — the last
//!     one proves `line_count` crossed in and the decorations cross back;
//!   - an empty buffer (`line_count == 0`) returns the WIT typed `err`,
//!     exercising the graceful "no decorations for this trigger" path (§8), which
//!     the host maps to keeping the buffer's prior cached snapshot (no flicker).

wit_bindgen::generate!({
    world: "decorations-plugin",
    path: "../../../../../wit",
});

use exports::lattice::plugin_host::decorations::Guest;
use lattice::plugin_host::types::{
    DecorationContext, GutterDecoration, GutterDiff, GutterDiffKind, GutterSeverity,
    GutterSeverityLevel,
};

struct Component;

impl Guest for Component {
    fn gutter_decorations(ctx: DecorationContext) -> Result<Vec<GutterDecoration>, String> {
        if ctx.line_count == 0 {
            // Graceful: nothing to decorate → a typed guest err, not a trap.
            return Err("empty buffer: no decorations".to_string());
        }
        Ok(vec![
            GutterDecoration::Diff(GutterDiff {
                line: 0,
                kind: GutterDiffKind::Change,
            }),
            GutterDecoration::Severity(GutterSeverity {
                line: 1,
                level: GutterSeverityLevel::Error,
            }),
            // Keyed off `line_count` — proves the context crossed in.
            GutterDecoration::Diff(GutterDiff {
                line: ctx.line_count - 1,
                kind: GutterDiffKind::Add,
            }),
        ])
    }
}

export!(Component);
