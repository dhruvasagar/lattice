//! The decoration/ui boundary conversions (plugin-host.md §5 `decorations`/`ui`,
//! PH7.9a).
//!
//! Mirrors `Mode::gutter_decorations` + `GutterDecoration` (lattice-mode) — the
//! per-line gutter data a plugin decoration provider produces. Two directions:
//!
//!   - **`GutterDecoration`** crosses **guest→host** (the producer's return): a
//!     `WitBoundary` round-trip (compiler-exhaustive both ways — a new arm can't
//!     land without a mapping; the `effect` precedent). Per-line scalars only; no
//!     draw calls cross.
//!   - **`decoration-context`** crosses **host→guest** (one-way, the grammar
//!     `project_*` precedent). The native `DecorationCtx` is `buffer_id` + a
//!     `ServiceRegistry` of render-state snapshots (host-owned, can't cross), and
//!     a plugin producer runs OFF the render path anyway — so the host builds the
//!     owned context from buffer metadata (id / path / line count) when it
//!     triggers the producer. Bulk buffer text rides `host-services` / the
//!     deferred `document` handle, not this record.
//!
//! The `ui` emit surface (segment / notification) is **type-mirror-only** (the
//! PH7.6 matcher/ranker precedent): the WIT records exist so the ABI is sized for
//! the freeze, but there is no guest→host emit producer in v1 (a plugin pushes
//! modeline content via the event-bus element path, ML.3, and notifications via
//! `effect.echo`). No native `UiSegment`/`UiNotification` type exists to convert
//! against, so no `WitBoundary` impl — only a construction smoke test.

use crate::WitBoundary;
use crate::lattice::plugin_host::types::{
    DecorationContext as WitDecorationContext, GutterDecoration as WitGutterDecoration,
    GutterDiff as WitGutterDiff, GutterDiffKind as WitGutterDiffKind,
    GutterSeverity as WitGutterSeverity, GutterSeverityLevel as WitGutterSeverityLevel,
};
use lattice_mode::{
    GutterDecoration as NativeGutterDecoration, GutterDiffKind as NativeGutterDiffKind,
    GutterSeverityLevel as NativeGutterSeverityLevel,
};

impl WitBoundary for NativeGutterDiffKind {
    type Wit = WitGutterDiffKind;

    fn to_wit(&self) -> Result<WitGutterDiffKind, String> {
        Ok(match self {
            NativeGutterDiffKind::Add => WitGutterDiffKind::Add,
            NativeGutterDiffKind::Remove => WitGutterDiffKind::Remove,
            NativeGutterDiffKind::Change => WitGutterDiffKind::Change,
            NativeGutterDiffKind::Conflict => WitGutterDiffKind::Conflict,
        })
    }

    fn from_wit(wit: WitGutterDiffKind) -> Result<Self, String> {
        Ok(match wit {
            WitGutterDiffKind::Add => NativeGutterDiffKind::Add,
            WitGutterDiffKind::Remove => NativeGutterDiffKind::Remove,
            WitGutterDiffKind::Change => NativeGutterDiffKind::Change,
            WitGutterDiffKind::Conflict => NativeGutterDiffKind::Conflict,
        })
    }
}

impl WitBoundary for NativeGutterSeverityLevel {
    type Wit = WitGutterSeverityLevel;

    fn to_wit(&self) -> Result<WitGutterSeverityLevel, String> {
        Ok(match self {
            NativeGutterSeverityLevel::Hint => WitGutterSeverityLevel::Hint,
            NativeGutterSeverityLevel::Info => WitGutterSeverityLevel::Info,
            NativeGutterSeverityLevel::Warning => WitGutterSeverityLevel::Warning,
            NativeGutterSeverityLevel::Error => WitGutterSeverityLevel::Error,
        })
    }

    fn from_wit(wit: WitGutterSeverityLevel) -> Result<Self, String> {
        Ok(match wit {
            WitGutterSeverityLevel::Hint => NativeGutterSeverityLevel::Hint,
            WitGutterSeverityLevel::Info => NativeGutterSeverityLevel::Info,
            WitGutterSeverityLevel::Warning => NativeGutterSeverityLevel::Warning,
            WitGutterSeverityLevel::Error => NativeGutterSeverityLevel::Error,
        })
    }
}

impl WitBoundary for NativeGutterDecoration {
    type Wit = WitGutterDecoration;

    /// Compiler-exhaustive: a new `GutterDecoration` arm forces a mapping here.
    fn to_wit(&self) -> Result<WitGutterDecoration, String> {
        Ok(match self {
            NativeGutterDecoration::Diff { line, kind } => {
                WitGutterDecoration::Diff(WitGutterDiff {
                    line: *line,
                    kind: kind.to_wit()?,
                })
            }
            NativeGutterDecoration::Severity { line, level } => {
                WitGutterDecoration::Severity(WitGutterSeverity {
                    line: *line,
                    level: level.to_wit()?,
                })
            }
        })
    }

    fn from_wit(wit: WitGutterDecoration) -> Result<Self, String> {
        Ok(match wit {
            WitGutterDecoration::Diff(d) => NativeGutterDecoration::Diff {
                line: d.line,
                kind: NativeGutterDiffKind::from_wit(d.kind)?,
            },
            WitGutterDecoration::Severity(s) => NativeGutterDecoration::Severity {
                line: s.line,
                level: NativeGutterSeverityLevel::from_wit(s.level)?,
            },
        })
    }
}

/// Build the owned `decoration-context` the host hands a producer (host→guest,
/// one-way). The host has the buffer metadata off the render path when it
/// triggers the producer; the guest computes per-line decorations from these
/// scalars (+ `host-services` for external data like git HEAD). A non-UTF-8 path
/// is dropped to `None` (a decoration producer keys off the *buffer*, not the
/// path text — losing an un-representable path degrades gracefully rather than
/// failing the whole trigger, unlike an event delivery).
pub fn project_decoration_context(
    buffer_id: u64,
    path: Option<&std::path::Path>,
    line_count: u32,
) -> WitDecorationContext {
    WitDecorationContext {
        buffer_id,
        path: path.and_then(|p| p.to_str().map(str::to_string)),
        line_count,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::lattice::plugin_host::types::{EchoLevel, UiNotification, UiSegment, UiZone};

    #[test]
    fn gutter_diff_kind_round_trips_every_arm() {
        for k in [
            NativeGutterDiffKind::Add,
            NativeGutterDiffKind::Remove,
            NativeGutterDiffKind::Change,
            NativeGutterDiffKind::Conflict,
        ] {
            assert_eq!(
                NativeGutterDiffKind::from_wit(k.to_wit().unwrap()).unwrap(),
                k
            );
        }
    }

    #[test]
    fn gutter_severity_level_round_trips_every_arm() {
        for l in [
            NativeGutterSeverityLevel::Hint,
            NativeGutterSeverityLevel::Info,
            NativeGutterSeverityLevel::Warning,
            NativeGutterSeverityLevel::Error,
        ] {
            assert_eq!(
                NativeGutterSeverityLevel::from_wit(l.to_wit().unwrap()).unwrap(),
                l
            );
        }
    }

    #[test]
    fn gutter_decoration_arms_round_trip() {
        let diff = NativeGutterDecoration::Diff {
            line: 12,
            kind: NativeGutterDiffKind::Change,
        };
        let back = NativeGutterDecoration::from_wit(diff.to_wit().unwrap()).unwrap();
        assert!(matches!(
            back,
            NativeGutterDecoration::Diff {
                line: 12,
                kind: NativeGutterDiffKind::Change
            }
        ));

        let sev = NativeGutterDecoration::Severity {
            line: 3,
            level: NativeGutterSeverityLevel::Error,
        };
        let back = NativeGutterDecoration::from_wit(sev.to_wit().unwrap()).unwrap();
        assert!(matches!(
            back,
            NativeGutterDecoration::Severity {
                line: 3,
                level: NativeGutterSeverityLevel::Error
            }
        ));
    }

    #[test]
    fn decoration_context_projects_metadata() {
        let ctx = project_decoration_context(9, Some(std::path::Path::new("src/lib.rs")), 240);
        assert_eq!(ctx.buffer_id, 9);
        assert_eq!(ctx.path.as_deref(), Some("src/lib.rs"));
        assert_eq!(ctx.line_count, 240);

        // A pathless (scratch) buffer projects `None`.
        let scratch = project_decoration_context(1, None, 0);
        assert!(scratch.path.is_none());
    }

    #[test]
    fn ui_type_mirror_is_constructible() {
        // Type-mirror-only (no emit producer in v1): assert the `ui` records
        // exist and are shaped correctly so the ABI is sized for the freeze.
        let seg = UiSegment {
            zone: UiZone::Right,
            text: "main*".to_string(),
            role: Some("modeline.branch".to_string()),
        };
        assert!(matches!(seg.zone, UiZone::Right));
        let note = UiNotification {
            level: EchoLevel::Warn,
            message: "plugin loaded with reduced function".to_string(),
        };
        assert!(matches!(note.level, EchoLevel::Warn));
    }
}
