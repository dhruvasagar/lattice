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
//! The `ui` emit surface **had** a modeline half here that was type-mirror-only,
//! and OC.3 / ML.6 gave it a real producer — `ui.wit` plus [`ui_host`], which
//! builds native `ModelineElement` / `ModelineElementUpdate` values directly
//! rather than round-tripping a record, so nothing here converts for it. The
//! `ui-segment` record that used to be mirrored is gone with it: building the
//! producer against a real consumer showed it conflated the descriptor's zone
//! with the content's text (see the note in `types.wit`).
//!
//! `ui-notification` is still mirror-only and still has no producer — a plugin
//! notifies via `effect.echo` — so the smoke test below keeps sizing it for the
//! freeze.
//!
//! [`ui_host`]: crate::ui_host

use crate::WitBoundary;
use crate::lattice::plugin_host::types::{
    DecorationContext as WitDecorationContext, GutterDecoration as WitGutterDecoration,
    GutterDiffKind as WitGutterDiffKind, GutterSeverityLevel as WitGutterSeverityLevel,
    GutterSign as WitGutterSign,
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

    /// SG.4b: every native decoration is a `Sign` now, and a sign carries an
    /// interned id whose NAME only the registry knows. This context-free
    /// conversion does not have one, so both directions refuse by naming the
    /// registry-aware pair rather than dropping the placement.
    ///
    /// The impl is kept rather than deleted because it is what makes that
    /// refusal a compiler-checked total function: a future arm still has to
    /// decide here, and "needs a registry" is a decision worth being told
    /// about rather than discovered as a missing glyph.
    fn to_wit(&self) -> Result<WitGutterDecoration, String> {
        match self {
            NativeGutterDecoration::Sign { .. } => Err(
                "a gutter sign placement needs the sign registry to name it — \
                 use `decoration_to_wit`"
                    .to_string(),
            ),
        }
    }

    fn from_wit(wit: WitGutterDecoration) -> Result<Self, String> {
        match wit {
            // `diff` and `severity` are sugar for a built-in sign's NAME, and
            // a name needs the registry exactly as much as the `sign` arm
            // does — so all three refuse identically here.
            WitGutterDecoration::Diff(_)
            | WitGutterDecoration::Severity(_)
            | WitGutterDecoration::Sign(_) => {
                Err("a gutter decoration needs the sign registry to resolve — \
                 use `decoration_from_wit`"
                    .to_string())
            }
        }
    }
}

/// SG.3b — the registry-aware guest→host conversion, and the one the producer
/// call site uses.
///
/// `Ok(None)` means the placement is SKIPPED, which happens for exactly one
/// reason: the guest named a sign nothing has defined. That is the same answer
/// the native render path gives an unknown id — paint nothing — and it is
/// deliberately not an `Err`, because an `Err` fails the whole batch and would
/// take the plugin's diff and severity marks down with it over one unregistered
/// name. A definition that has not registered yet is recoverable; a malformed
/// record is not, and those still fail.
///
/// The resolution happens HERE, at the boundary, off the render path — which is
/// the whole reason a native placement can stay `Copy` and carry no per-line
/// `String`.
pub fn decoration_from_wit(
    wit: WitGutterDecoration,
    registry: &lattice_mode::SignRegistry,
) -> Result<Option<NativeGutterDecoration>, String> {
    // SG.4b: the three arms differ only in how the sign is NAMED — a `sign`
    // arm names it directly, the other two name a built-in. Past that they are
    // the same placement, which is the whole point of the unification.
    let (line, name): (u32, &str) = match &wit {
        WitGutterDecoration::Diff(d) => (d.line, builtin_sign_name_for_diff(d.kind)),
        WitGutterDecoration::Severity(s) => (s.line, builtin_sign_name_for_severity(s.level)),
        WitGutterDecoration::Sign(s) => (s.line, s.name.as_str()),
    };
    let Some(sign) = registry.id_of(name) else {
        // `debug!`, not `warn!`: a decoration producer runs on every refresh,
        // so a guest with one bad name would flood the log at keystroke rate
        // and bury everything else.
        //
        // A `diff` / `severity` arm reaching here means the host never
        // registered its built-ins — a stripped harness rather than a guest
        // bug — and the same skip is the right answer either way: no mark,
        // rather than a mark resolving to something else.
        tracing::debug!(
            sign = %name,
            line,
            "gutter sign placement skipped: no such sign is defined"
        );
        return Ok(None);
    };
    Ok(Some(NativeGutterDecoration::Sign { line, sign }))
}

/// SG.4b — the built-in sign a WIT `diff` arm names.
///
/// The wire keeps `diff` and `severity` as sugar. A guest saying "line 4 is an
/// addition" should not have to know the host spells that `diff.add`, and
/// deleting the arms would break every decoration plugin for a change entirely
/// internal to the host. The mapping lives here, at the boundary, so the native
/// side has exactly one kind of gutter decoration.
fn builtin_sign_name_for_diff(kind: WitGutterDiffKind) -> &'static str {
    match kind {
        WitGutterDiffKind::Add => "diff.add",
        WitGutterDiffKind::Remove => "diff.remove",
        WitGutterDiffKind::Change => "diff.change",
        WitGutterDiffKind::Conflict => "diff.conflict",
    }
}

/// The built-in sign a WIT `severity` arm names. Peer of
/// [`builtin_sign_name_for_diff`].
fn builtin_sign_name_for_severity(level: WitGutterSeverityLevel) -> &'static str {
    match level {
        WitGutterSeverityLevel::Hint => "diagnostic.hint",
        WitGutterSeverityLevel::Info => "diagnostic.info",
        WitGutterSeverityLevel::Warning => "diagnostic.warning",
        WitGutterSeverityLevel::Error => "diagnostic.error",
    }
}

/// SG.3b — the registry-aware host→guest conversion.
///
/// The mirror of [`decoration_from_wit`], for the direction that has no
/// consumer yet: nothing in the host sends native decorations to a guest. It
/// exists so the round trip is testable as a round trip — a name that survives
/// out and back is the property that matters, and testing only one direction
/// would not catch an id/name mapping that silently disagreed with itself.
///
/// A retired id has no name to send, so it converts to `None` rather than an
/// error, matching the inbound direction's treatment of an unknown name.
pub fn decoration_to_wit(
    deco: &NativeGutterDecoration,
    registry: &lattice_mode::SignRegistry,
) -> Result<Option<WitGutterDecoration>, String> {
    match deco {
        NativeGutterDecoration::Sign { line, sign } => {
            let Some(def) = registry.get(*sign) else {
                return Ok(None);
            };
            // Always the `sign` arm, even for a built-in: the wire's `diff` /
            // `severity` arms are inbound sugar, and answering with the name
            // keeps the round trip exact rather than lossy through a second
            // spelling of the same thing.
            Ok(Some(WitGutterDecoration::Sign(WitGutterSign {
                line: *line,
                name: def.name.clone(),
            })))
        }
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
    // SG.4b: the `diff` / `severity` payload records are only CONSTRUCTED by
    // tests now — the production path names a built-in sign rather than
    // building one — so they are imported here rather than at module scope.
    use crate::lattice::plugin_host::types::{
        EchoLevel, GutterDiff as WitGutterDiff, GutterSeverity as WitGutterSeverity, UiNotification,
    };

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

    /// SG.4b: the wire keeps its `diff` and `severity` arms, and they resolve
    /// to the BUILT-IN signs. A guest saying "line 4 is an addition" does not
    /// have to know the host spells that `diff.add` — which is the whole
    /// reason the arms were kept as sugar rather than deleted with the native
    /// variants.
    #[test]
    fn the_wire_sugar_arms_resolve_to_builtin_signs() {
        let mut registry = lattice_mode::SignRegistry::new();
        let ids = lattice_mode::register_builtin_signs(
            &mut registry,
            lattice_mode::DiagnosticGlyphs::default(),
        );

        let diff = WitGutterDecoration::Diff(WitGutterDiff {
            line: 12,
            kind: WitGutterDiffKind::Change,
        });
        assert_eq!(
            decoration_from_wit(diff, &registry).unwrap(),
            Some(NativeGutterDecoration::Sign {
                line: 12,
                sign: ids.diff_change
            })
        );

        let sev = WitGutterDecoration::Severity(WitGutterSeverity {
            line: 3,
            level: WitGutterSeverityLevel::Error,
        });
        assert_eq!(
            decoration_from_wit(sev, &registry).unwrap(),
            Some(NativeGutterDecoration::Sign {
                line: 3,
                sign: ids.diagnostic_error
            })
        );
    }

    /// A host that never registered its built-ins skips a sugar arm rather
    /// than resolving it to something else — a stripped harness, not a guest
    /// bug, and no mark is the honest answer.
    #[test]
    fn a_sugar_arm_without_registered_builtins_is_skipped() {
        let registry = lattice_mode::SignRegistry::new();
        let diff = WitGutterDecoration::Diff(WitGutterDiff {
            line: 1,
            kind: WitGutterDiffKind::Add,
        });
        assert!(decoration_from_wit(diff, &registry).unwrap().is_none());
    }

    fn sign_registry_with(names: &[(&str, i32)]) -> lattice_mode::SignRegistry {
        let mut r = lattice_mode::SignRegistry::new();
        for (name, priority) in names {
            r.define(lattice_mode::SignDefinition {
                name: (*name).to_string(),
                text: "\u{f111}".into(),
                fallback: "●".into(),
                theme_element: format!("{name}.element"),
                priority: *priority,
                column: lattice_mode::SIGN_COLUMN_MARK.to_string(),
            });
        }
        r
    }

    /// SG.3b: the whole point of the wire format is that a NAME survives out
    /// and back as the SAME interned id. Testing one direction would not catch
    /// an id↔name mapping that silently disagreed with itself.
    #[test]
    fn a_sign_placement_round_trips_through_its_name() {
        let registry = sign_registry_with(&[("debugger.breakpoint", 20)]);
        let id = registry.id_of("debugger.breakpoint").unwrap();
        let native = NativeGutterDecoration::Sign { line: 7, sign: id };

        let wit = decoration_to_wit(&native, &registry)
            .unwrap()
            .expect("a live id has a name to send");
        match &wit {
            WitGutterDecoration::Sign(s) => {
                assert_eq!(s.line, 7);
                assert_eq!(
                    s.name, "debugger.breakpoint",
                    "the NAME crosses, not the id"
                );
            }
            other => panic!("expected a sign arm, got {other:?}"),
        }

        let back = decoration_from_wit(wit, &registry)
            .unwrap()
            .expect("a defined name resolves");
        assert_eq!(back, native, "and it resolves to the SAME interned id");
    }

    /// A name nothing has defined is SKIPPED, not an error — because an error
    /// fails the whole batch and would take the plugin's diff and severity
    /// marks down with it over one unregistered name.
    #[test]
    fn an_unknown_sign_name_is_skipped_and_the_batch_survives() {
        let mut registry = sign_registry_with(&[("debugger.breakpoint", 20)]);
        // SG.4b: the neighbour below is a `diff` arm, which is SUGAR for a
        // built-in sign's name — so the built-ins have to be registered for it
        // to resolve at all. Without them this test would assert "the batch
        // survives" against a batch where nothing survived.
        lattice_mode::register_builtin_signs(
            &mut registry,
            lattice_mode::DiagnosticGlyphs::default(),
        );
        let unknown = WitGutterDecoration::Sign(WitGutterSign {
            line: 3,
            name: "debugger.nope".to_string(),
        });
        assert!(decoration_from_wit(unknown, &registry).unwrap().is_none());

        // The neighbour in the same batch still crosses — this is the half
        // that would be lost if the unknown name had been an `Err`.
        let diff = WitGutterDecoration::Diff(WitGutterDiff {
            line: 4,
            kind: WitGutterDiffKind::Add,
        });
        assert!(decoration_from_wit(diff, &registry).unwrap().is_some());
    }

    /// A retired id has no name to send. `None` rather than an error, matching
    /// how the inbound direction treats an unknown name — and matching the
    /// render path, where a retired id paints nothing.
    #[test]
    fn a_retired_id_has_no_name_to_send() {
        let mut registry = sign_registry_with(&[("debugger.breakpoint", 20)]);
        let id = registry.id_of("debugger.breakpoint").unwrap();
        registry.undefine("debugger.breakpoint");
        let native = NativeGutterDecoration::Sign { line: 1, sign: id };
        assert!(decoration_to_wit(&native, &registry).unwrap().is_none());
    }

    /// The context-free `WitBoundary` conversions cannot spell a sign, and say
    /// so by name rather than dropping it. The boundary's contract is that a
    /// new arm forces a decision at every site; "needs the registry" is a
    /// decision worth being told about rather than discovering as a missing
    /// glyph.
    #[test]
    fn the_registry_free_conversions_refuse_a_sign_by_name() {
        let registry = sign_registry_with(&[("p.mark", 5)]);
        let id = registry.id_of("p.mark").unwrap();
        let err = NativeGutterDecoration::Sign { line: 0, sign: id }
            .to_wit()
            .expect_err("no registry, no name");
        assert!(err.contains("decoration_to_wit"), "{err}");

        let err = NativeGutterDecoration::from_wit(WitGutterDecoration::Sign(WitGutterSign {
            line: 0,
            name: "p.mark".to_string(),
        }))
        .expect_err("no registry, no id");
        assert!(err.contains("decoration_from_wit"), "{err}");
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
        // Still mirror-only (no emit producer): assert the record exists and is
        // shaped correctly so the ABI stays sized for the freeze. The modeline
        // half of `ui` grew a real producer at OC.3 and is covered by
        // `ui_host`'s own tests and `tests/modeline_seam.rs`.
        let note = UiNotification {
            level: EchoLevel::Warn,
            message: "plugin loaded with reduced function".to_string(),
        };
        assert!(matches!(note.level, EchoLevel::Warn));
    }
}
