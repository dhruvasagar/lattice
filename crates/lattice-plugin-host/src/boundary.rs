//! The boundary adapter machinery (plugin-host.md §4).
//!
//! Design fragment §4. Slice: PH7.3a (conventions + `Args`); the `Effect`
//! variant mirror is PH7.3b; the `document` resource handle + owned-snapshot
//! projection is PH7.3c.
//!
//! Native host types (`lattice_grammar::args::Args`, `Effect`, the picker /
//! completion records) cannot cross a WASM boundary as-is — they carry
//! borrows, closures, `Future`/`Stream` carriers, or non-serde fields (§4.1–
//! §4.5). The **generated WIT mirror** (from `wit/types.wit`, emitted by
//! `bindgen!` at the crate root) is the owned, serializable shape the guest
//! sees. [`WitBoundary`] is the single adapter contract between the two: one
//! place that defines `to_wit` / `from_wit` and the `Result<_, String>` error
//! convention, so every type round-trips uniformly and a malformed payload is
//! rejected at the boundary rather than at apply-time.
//!
//! The `String` error arm is deliberate: it is the WIT `result<_, string>`
//! convention (§4, "Result<_, string> error convention"), so a conversion that
//! cannot be represented yet (e.g. a nested `CommandInvocation`, §4.1) surfaces
//! as a typed error the host logs + skips, never a panic or a lossy encoding.

// The generated WIT mirrors live at the crate root (the `bindgen!` site);
// alias them so native/`Wit` pairs read unambiguously.
use std::path::{Path, PathBuf};

// All WIT types the `types` interface defines live under this generated
// module; the world-level `use` also surfaces a few at the crate root, but the
// transitively-referenced payload records only exist here, so import uniformly.
use crate::lattice::plugin_host::types::{
    ArgValue as WitArgValue, Args as WitArgs, CandidateChord as WitCandidateChord,
    CandidateData as WitCandidateData, CandidateExtension as WitCandidateExtension,
    CandidateFile as WitCandidateFile, CandidateKind as WitCandidateKind,
    CandidateMark as WitCandidateMark, CandidateOption as WitCandidateOption,
    CandidateOptionValue as WitCandidateOptionValue, CandidateRegister as WitCandidateRegister,
    CommandRef as WitCommandRef, JumpTarget as WitJumpTarget, Location as WitLocation,
    LspCodeActionRef as WitLspCodeActionRef, PickerAcceptOutcome as WitPickerAcceptOutcome,
    RawCandidate as WitRawCandidate,
};
use lattice_completion::candidate::{
    CandidateData as NativeCandidateData, CandidateKind as NativeCandidateKind,
    RawCandidate as NativeRawCandidate,
};
use lattice_completion::insert::SourceId;
use lattice_grammar::args::{ArgValue as NativeArgValue, Args as NativeArgs};
use lattice_picker::outcome::PickerAcceptOutcome as NativePickerAcceptOutcome;

/// A host path crosses the boundary as a WIT `string`, which must be UTF-8. A
/// non-UTF-8 path cannot cross faithfully, so it is a typed error rather than a
/// lossy `to_string_lossy`.
pub(crate) fn path_to_wit(path: &Path) -> Result<String, String> {
    path.to_str().map(str::to_string).ok_or_else(|| {
        format!(
            "path is not valid UTF-8 and cannot cross the boundary: {}",
            path.display()
        )
    })
}

/// Converts a native host type to/from its generated WIT mirror.
///
/// `to_wit` borrows the native value (the host owns it); `from_wit` consumes
/// the WIT value (it arrived by value across the boundary). Both return
/// `Result<_, String>` — the WIT `result<_, string>` convention — so a value
/// that cannot be represented in the current WIT surface is a typed error, not
/// a panic or a silent drop.
pub trait WitBoundary: Sized {
    /// The generated WIT mirror type.
    type Wit;

    /// Project the native value into its owned WIT mirror.
    fn to_wit(&self) -> Result<Self::Wit, String>;

    /// Reconstruct the native value from its WIT mirror.
    fn from_wit(wit: Self::Wit) -> Result<Self, String>;
}

impl WitBoundary for NativeArgValue {
    type Wit = WitArgValue;

    fn to_wit(&self) -> Result<WitArgValue, String> {
        Ok(match self {
            NativeArgValue::String(s) => WitArgValue::String(s.clone()),
            NativeArgValue::Char(c) => WitArgValue::Char(*c),
            NativeArgValue::Bool(b) => WitArgValue::Bool(*b),
            NativeArgValue::Int(i) => WitArgValue::Int(*i),
            NativeArgValue::Pattern(s) => WitArgValue::Pattern(s.clone()),
            NativeArgValue::Chord(s) => WitArgValue::Chord(s.clone()),
            NativeArgValue::Raw(s) => WitArgValue::Raw(s.clone()),
            // A nested invocation needs the command mirror (§4.1); until then
            // it crosses as a typed error rather than a lossy string.
            NativeArgValue::Invocation(_) => {
                return Err(
                    "ArgValue::Invocation crosses the boundary with the command mirror \
                     (PH7.3b, fragment §4.1)"
                        .to_string(),
                );
            }
        })
    }

    fn from_wit(wit: WitArgValue) -> Result<Self, String> {
        Ok(match wit {
            WitArgValue::String(s) => NativeArgValue::String(s),
            WitArgValue::Char(c) => NativeArgValue::Char(c),
            WitArgValue::Bool(b) => NativeArgValue::Bool(b),
            WitArgValue::Int(i) => NativeArgValue::Int(i),
            WitArgValue::Pattern(s) => NativeArgValue::Pattern(s),
            WitArgValue::Chord(s) => NativeArgValue::Chord(s),
            WitArgValue::Raw(s) => NativeArgValue::Raw(s),
        })
    }
}

impl WitBoundary for NativeArgs {
    type Wit = WitArgs;

    fn to_wit(&self) -> Result<WitArgs, String> {
        Ok(match self {
            NativeArgs::None => WitArgs::None,
            NativeArgs::Char(c) => WitArgs::Char(*c),
            NativeArgs::String(s) => WitArgs::String(s.clone()),
            NativeArgs::Bytes(b) => WitArgs::Bytes(b.clone()),
            NativeArgs::List(values) => WitArgs::List(
                values
                    .iter()
                    .map(WitBoundary::to_wit)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        })
    }

    fn from_wit(wit: WitArgs) -> Result<Self, String> {
        Ok(match wit {
            WitArgs::None => NativeArgs::None,
            WitArgs::Char(c) => NativeArgs::Char(c),
            WitArgs::String(s) => NativeArgs::String(s),
            WitArgs::Bytes(b) => NativeArgs::Bytes(b),
            WitArgs::List(values) => NativeArgs::List(
                values
                    .into_iter()
                    .map(NativeArgValue::from_wit)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        })
    }
}

impl WitBoundary for NativeCandidateKind {
    type Wit = WitCandidateKind;

    fn to_wit(&self) -> Result<WitCandidateKind, String> {
        Ok(match self {
            NativeCandidateKind::Command => WitCandidateKind::Command,
            NativeCandidateKind::Option => WitCandidateKind::Option,
            NativeCandidateKind::File => WitCandidateKind::File,
            NativeCandidateKind::Directory => WitCandidateKind::Directory,
            NativeCandidateKind::Pattern => WitCandidateKind::Pattern,
            NativeCandidateKind::Buffer => WitCandidateKind::Buffer,
            NativeCandidateKind::Register => WitCandidateKind::Register,
            NativeCandidateKind::Mark => WitCandidateKind::Mark,
            NativeCandidateKind::Chord => WitCandidateKind::Chord,
            NativeCandidateKind::Plain => WitCandidateKind::Plain,
            NativeCandidateKind::Extension(tag) => WitCandidateKind::Extension(*tag),
        })
    }

    fn from_wit(wit: WitCandidateKind) -> Result<Self, String> {
        Ok(match wit {
            WitCandidateKind::Command => NativeCandidateKind::Command,
            WitCandidateKind::Option => NativeCandidateKind::Option,
            WitCandidateKind::File => NativeCandidateKind::File,
            WitCandidateKind::Directory => NativeCandidateKind::Directory,
            WitCandidateKind::Pattern => NativeCandidateKind::Pattern,
            WitCandidateKind::Buffer => NativeCandidateKind::Buffer,
            WitCandidateKind::Register => NativeCandidateKind::Register,
            WitCandidateKind::Mark => NativeCandidateKind::Mark,
            WitCandidateKind::Chord => NativeCandidateKind::Chord,
            WitCandidateKind::Plain => NativeCandidateKind::Plain,
            WitCandidateKind::Extension(tag) => NativeCandidateKind::Extension(tag),
        })
    }
}

impl WitBoundary for NativeCandidateData {
    type Wit = WitCandidateData;

    fn to_wit(&self) -> Result<WitCandidateData, String> {
        Ok(match self {
            // `Command` carries a recursive `SourceLocation` (§4.4); it is a
            // native-generator concern and crosses only once the provenance
            // mirror lands — a typed error, never a lossy encoding.
            NativeCandidateData::Command { .. } => {
                return Err(
                    "CandidateData::Command carries a recursive SourceLocation; it crosses \
                     with the provenance mirror (fragment §4.4)"
                        .to_string(),
                );
            }
            NativeCandidateData::File { path, is_dir, size } => {
                WitCandidateData::File(WitCandidateFile {
                    path: path_to_wit(path)?,
                    is_dir: *is_dir,
                    size: *size,
                })
            }
            NativeCandidateData::Option {
                name,
                current_value,
                doc,
            } => WitCandidateData::Option(WitCandidateOption {
                name: name.clone(),
                current_value: current_value.clone(),
                doc: doc.clone(),
            }),
            NativeCandidateData::OptionValue {
                option_name,
                value,
                doc,
            } => WitCandidateData::OptionValue(WitCandidateOptionValue {
                option_name: option_name.clone(),
                value: value.clone(),
                doc: doc.clone(),
            }),
            NativeCandidateData::Chord {
                chord,
                mode_label,
                doc,
            } => WitCandidateData::Chord(WitCandidateChord {
                chord: chord.clone(),
                mode_label: mode_label.clone(),
                doc: doc.clone(),
            }),
            NativeCandidateData::Register { name, preview } => {
                WitCandidateData::Register(WitCandidateRegister {
                    name: *name,
                    preview: preview.clone(),
                })
            }
            NativeCandidateData::Mark { name, position } => {
                WitCandidateData::Mark(WitCandidateMark {
                    name: *name,
                    position: position.clone(),
                })
            }
            NativeCandidateData::Plain => WitCandidateData::Plain,
            NativeCandidateData::Extension { kind_id, payload } => {
                WitCandidateData::Extension(WitCandidateExtension {
                    kind_id: *kind_id,
                    payload: payload.clone(),
                })
            }
        })
    }

    fn from_wit(wit: WitCandidateData) -> Result<Self, String> {
        Ok(match wit {
            WitCandidateData::File(f) => NativeCandidateData::File {
                path: PathBuf::from(f.path),
                is_dir: f.is_dir,
                size: f.size,
            },
            WitCandidateData::Option(o) => NativeCandidateData::Option {
                name: o.name,
                current_value: o.current_value,
                doc: o.doc,
            },
            WitCandidateData::OptionValue(o) => NativeCandidateData::OptionValue {
                option_name: o.option_name,
                value: o.value,
                doc: o.doc,
            },
            WitCandidateData::Chord(c) => NativeCandidateData::Chord {
                chord: c.chord,
                mode_label: c.mode_label,
                doc: c.doc,
            },
            WitCandidateData::Register(r) => NativeCandidateData::Register {
                name: r.name,
                preview: r.preview,
            },
            WitCandidateData::Mark(m) => NativeCandidateData::Mark {
                name: m.name,
                position: m.position,
            },
            WitCandidateData::Plain => NativeCandidateData::Plain,
            WitCandidateData::Extension(e) => NativeCandidateData::Extension {
                kind_id: e.kind_id,
                payload: e.payload,
            },
        })
    }
}

impl WitBoundary for NativeRawCandidate {
    type Wit = WitRawCandidate;

    fn to_wit(&self) -> Result<WitRawCandidate, String> {
        Ok(WitRawCandidate {
            text: self.text.clone(),
            insert_text: self.insert_text.clone(),
            display: self.display.clone(),
            source: self.source.as_ref().map(|s| s.0.clone()),
            kind: self.kind.to_wit()?,
            data: self.data.to_wit()?,
            // PH7.4a: marginalia crosses so plugin sources contribute themed
            // columns (`Annotation` boundary in `boundary_picker`).
            annotations: self
                .annotations
                .iter()
                .map(WitBoundary::to_wit)
                .collect::<Result<Vec<_>, String>>()?,
            // PS.1: host→guest carries the RANGES but cannot carry the styles —
            // a `Style` is a closed enum plus an interned element id, and the
            // NAME it was resolved from is not recoverable from it. This
            // direction is only exercised by the round-trip test and by a host
            // handing a candidate back for re-ranking, so an empty slot is the
            // honest answer rather than a fabricated name.
            display_spans: Vec::new(),
        })
    }

    fn from_wit(wit: WitRawCandidate) -> Result<Self, String> {
        // PS.1: resolved before `display` is moved into the record below.
        let spans = display_spans_from_wit(&wit.display, wit.display_spans);
        Ok(NativeRawCandidate {
            text: wit.text,
            insert_text: wit.insert_text,
            display: wit.display,
            source: wit.source.map(SourceId),
            kind: NativeCandidateKind::from_wit(wit.kind)?,
            data: NativeCandidateData::from_wit(wit.data)?,
            annotations: wit
                .annotations
                .into_iter()
                .map(lattice_completion::candidate::Annotation::from_wit)
                .collect::<Result<Vec<_>, String>>()?,
            // `accept_action` stays host-only (§4.4) — reconstructed empty and
            // re-derived host-side.
            accept_action: None,
            // PS.1: styled runs, resolved HERE because this is where the theme
            // is in hand and where a malformed span can be dropped once rather
            // than defended against at every paint site.
            display_spans: spans,
        })
    }
}

/// PS.1: resolve a guest's styled runs against its `display` text.
///
/// Every span is validated and an invalid one is **dropped** — not clamped,
/// not fatal. Dropping is the right severity for each failure mode:
///
/// - **Inverted or out of bounds** — the guest computed offsets against text it
///   did not send. Clamping would paint a run it never asked for, which is a
///   worse answer than painting none.
/// - **Not on a UTF-8 boundary** — this is the one that makes validation
///   non-optional rather than tidy. Slicing there panics, and a guest computing
///   offsets in `chars` instead of bytes is an ordinary bug that must not be
///   able to take the picker down.
/// - **An unresolvable `slot`** — kept, and rendered unstyled. The run is where
///   the guest said it was; only its colour is unknown, and a theme element
///   that is not registered yet is a normal transient state, not an error.
///
/// The row survives all of them: a picker that shows nothing because one span
/// was malformed is a worse failure than one that shows a plain row.
fn display_spans_from_wit(
    // Named `text` rather than `display`: inside `tracing::warn!` the bare name
    // `display` resolves to `tracing::field::display`, so the parameter would
    // shadow into a function item at every use site in the macro body.
    text: &str,
    spans: Vec<crate::lattice::plugin_host::types::DisplaySpan>,
) -> Vec<lattice_completion::candidate::DisplaySpan> {
    spans
        .into_iter()
        .filter_map(|s| {
            let (start, end) = (s.start as usize, s.end as usize);
            if start >= end || end > text.len() {
                tracing::warn!(
                    start,
                    end,
                    len = text.len(),
                    slot = %s.slot,
                    "picker candidate: display span out of range; dropped"
                );
                return None;
            }
            if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                tracing::warn!(
                    start,
                    end,
                    slot = %s.slot,
                    "picker candidate: display span is not on a UTF-8 boundary \
                     (offsets are BYTES, not chars); dropped"
                );
                return None;
            }
            Some(lattice_completion::candidate::DisplaySpan {
                range: start..end,
                style: resolve_slot_style(&s.slot),
            })
        })
        .collect()
}

/// PS.1: a `slot` name → `Style`, through exactly the path a `highlights.scm`
/// capture takes.
///
/// The sameness IS the feature. A builtin category (`keyword`,
/// `text.title.1`) resolves first, so a plugin cannot redefine what `keyword`
/// means for the whole editor; any other name resolves against the live theme
/// registry as a `Style::Element`, which is how a plugin's own registered
/// element reaches a picker row. So a row's colour tracks the active
/// colourscheme and matches the same construct rendered in a buffer, rather
/// than being a second palette that drifts out of step with it.
fn resolve_slot_style(slot: &str) -> lattice_cells::style::Style {
    lattice_syntax::style::name_to_style_with_theme(slot, None)
}

impl WitBoundary for NativePickerAcceptOutcome {
    type Wit = WitPickerAcceptOutcome;

    fn to_wit(&self) -> Result<WitPickerAcceptOutcome, String> {
        Ok(match self {
            NativePickerAcceptOutcome::OpenFile { path } => {
                WitPickerAcceptOutcome::OpenFile(path_to_wit(path)?)
            }
            NativePickerAcceptOutcome::SwitchBuffer { buffer_id } => {
                WitPickerAcceptOutcome::SwitchBuffer(*buffer_id)
            }
            NativePickerAcceptOutcome::JumpInBuffer {
                buffer_id,
                line,
                col,
            } => WitPickerAcceptOutcome::JumpInBuffer(WitJumpTarget {
                buffer_id: *buffer_id,
                line: *line,
                col: *col,
            }),
            NativePickerAcceptOutcome::JumpToMark { name } => {
                WitPickerAcceptOutcome::JumpToMark(*name)
            }
            NativePickerAcceptOutcome::JumpToLocation { path, line, col } => {
                WitPickerAcceptOutcome::JumpToLocation(WitLocation {
                    path: path_to_wit(path)?,
                    line: *line,
                    col: *col,
                })
            }
            NativePickerAcceptOutcome::InvokeCommand { id, args } => {
                WitPickerAcceptOutcome::InvokeCommand(WitCommandRef {
                    id: id.clone(),
                    args: args.to_wit()?,
                })
            }
            NativePickerAcceptOutcome::PasteRegister { name } => {
                WitPickerAcceptOutcome::PasteRegister(*name)
            }
            NativePickerAcceptOutcome::ExpandSnippet { id } => {
                WitPickerAcceptOutcome::ExpandSnippet(id.clone())
            }
            NativePickerAcceptOutcome::OpenLspLog { server_id } => {
                WitPickerAcceptOutcome::OpenLspLog(server_id.clone())
            }
            NativePickerAcceptOutcome::OpenLspTraceLog { server_id } => {
                WitPickerAcceptOutcome::OpenLspTraceLog(server_id.clone())
            }
            NativePickerAcceptOutcome::ApplyLspCodeAction { handle, index } => {
                WitPickerAcceptOutcome::ApplyLspCodeAction(WitLspCodeActionRef {
                    handle: *handle,
                    index: *index,
                })
            }
            NativePickerAcceptOutcome::ApplyLspCompletion { index } => {
                WitPickerAcceptOutcome::ApplyLspCompletion(*index)
            }
            NativePickerAcceptOutcome::ApplyColorscheme { name } => {
                WitPickerAcceptOutcome::ApplyColorscheme(name.clone())
            }
            // MB.3: `LoadCommandLine` seeds the `:` line — a host-internal
            // outcome for the native `history` picker. It is not part of the
            // plugin WIT surface (a plugin picker source has no business
            // driving the command line), so it never crosses the boundary.
            NativePickerAcceptOutcome::LoadCommandLine { .. }
            | NativePickerAcceptOutcome::LoadSearchLine { .. } => {
                return Err(
                    "load-command/load-search are host-internal picker outcomes, not representable over WIT"
                        .into(),
                );
            }
            NativePickerAcceptOutcome::OpenPrompt { .. } => {
                return Err(
                    "open-prompt is a host-internal picker outcome, not representable over WIT"
                        .into(),
                );
            }
            // YR.3: `fill-caller` puts text into a HOST surface — the
            // document, the `:` line, a prompt, a parked transient
            // argument. Which one is the `FillTarget` the host captured
            // when the picker opened, so a plugin source emitting this
            // would be filling something it cannot see or name.
            NativePickerAcceptOutcome::FillCaller { .. } => {
                return Err(
                    "fill-caller is a host-internal picker outcome, not representable over WIT"
                        .into(),
                );
            }
            NativePickerAcceptOutcome::NoOp => WitPickerAcceptOutcome::NoOp,
        })
    }

    fn from_wit(wit: WitPickerAcceptOutcome) -> Result<Self, String> {
        Ok(match wit {
            WitPickerAcceptOutcome::OpenFile(path) => NativePickerAcceptOutcome::OpenFile {
                path: PathBuf::from(path),
            },
            WitPickerAcceptOutcome::SwitchBuffer(buffer_id) => {
                NativePickerAcceptOutcome::SwitchBuffer { buffer_id }
            }
            WitPickerAcceptOutcome::JumpInBuffer(t) => NativePickerAcceptOutcome::JumpInBuffer {
                buffer_id: t.buffer_id,
                line: t.line,
                col: t.col,
            },
            WitPickerAcceptOutcome::JumpToMark(name) => {
                NativePickerAcceptOutcome::JumpToMark { name }
            }
            WitPickerAcceptOutcome::JumpToLocation(l) => {
                NativePickerAcceptOutcome::JumpToLocation {
                    path: PathBuf::from(l.path),
                    line: l.line,
                    col: l.col,
                }
            }
            WitPickerAcceptOutcome::InvokeCommand(c) => NativePickerAcceptOutcome::InvokeCommand {
                id: c.id,
                args: NativeArgs::from_wit(c.args)?,
            },
            WitPickerAcceptOutcome::PasteRegister(name) => {
                NativePickerAcceptOutcome::PasteRegister { name }
            }
            WitPickerAcceptOutcome::ExpandSnippet(id) => {
                NativePickerAcceptOutcome::ExpandSnippet { id }
            }
            WitPickerAcceptOutcome::OpenLspLog(server_id) => {
                NativePickerAcceptOutcome::OpenLspLog { server_id }
            }
            WitPickerAcceptOutcome::OpenLspTraceLog(server_id) => {
                NativePickerAcceptOutcome::OpenLspTraceLog { server_id }
            }
            WitPickerAcceptOutcome::ApplyLspCodeAction(r) => {
                NativePickerAcceptOutcome::ApplyLspCodeAction {
                    handle: r.handle,
                    index: r.index,
                }
            }
            WitPickerAcceptOutcome::ApplyLspCompletion(index) => {
                NativePickerAcceptOutcome::ApplyLspCompletion { index }
            }
            WitPickerAcceptOutcome::ApplyColorscheme(name) => {
                NativePickerAcceptOutcome::ApplyColorscheme { name }
            }
            WitPickerAcceptOutcome::NoOp => NativePickerAcceptOutcome::NoOp,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    /// native → WIT → native is the identity for every representable value.
    fn assert_args_round_trips(native: NativeArgs) {
        let wit = native.to_wit().expect("to_wit");
        let back = NativeArgs::from_wit(wit).expect("from_wit");
        assert_eq!(native, back);
    }

    #[test]
    fn args_round_trip_covers_every_representable_variant() {
        assert_args_round_trips(NativeArgs::None);
        assert_args_round_trips(NativeArgs::Char('x'));
        assert_args_round_trips(NativeArgs::String("hello".into()));
        assert_args_round_trips(NativeArgs::Bytes(vec![0, 1, 2, 255]));
        assert_args_round_trips(NativeArgs::List(vec![
            NativeArgValue::String("s".into()),
            NativeArgValue::Char('c'),
            NativeArgValue::Bool(true),
            NativeArgValue::Int(-42),
            NativeArgValue::Pattern("re".into()),
            NativeArgValue::Chord("<C-c>".into()),
            NativeArgValue::Raw("body".into()),
        ]));
    }

    #[test]
    fn arg_value_int_survives_full_i64_range() {
        for i in [i64::MIN, -1, 0, 1, i64::MAX] {
            let v = NativeArgValue::Int(i);
            let back = NativeArgValue::from_wit(v.to_wit().unwrap()).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn nested_invocation_is_a_typed_error_not_a_panic() {
        use lattice_grammar::CommandId;
        use lattice_grammar::command::CommandInvocation;
        // A nested invocation cannot cross until the command mirror lands; it
        // must surface as a typed Err, and a containing Args must propagate it.
        let invocation = CommandInvocation::of(CommandId::new(0));
        let native = NativeArgs::List(vec![NativeArgValue::Invocation(Box::new(invocation))]);
        let err = native
            .to_wit()
            .expect_err("nested invocation must not cross yet");
        assert!(err.contains("Invocation"), "error names the culprit: {err}");
    }

    // ---- RawCandidate / CandidateData / CandidateKind ----

    fn raw(kind: NativeCandidateKind, data: NativeCandidateData) -> NativeRawCandidate {
        NativeRawCandidate {
            insert_text: None,
            text: "text".into(),
            display: "display".into(),
            source: Some(SourceId("gen:files".into())),
            kind,
            data,
            // Non-crossable render-time fields — left at their reconstructed
            // defaults so the round-trip is an equality.
            accept_action: None,
            annotations: Vec::new(),
            display_spans: Vec::new(),
        }
    }

    fn assert_candidate_round_trips(native: NativeRawCandidate) {
        let wit = native.to_wit().expect("to_wit");
        let back = NativeRawCandidate::from_wit(wit).expect("from_wit");
        assert_eq!(native, back);
    }

    #[test]
    fn raw_candidate_round_trips_across_data_and_kind_variants() {
        assert_candidate_round_trips(raw(NativeCandidateKind::Plain, NativeCandidateData::Plain));
        assert_candidate_round_trips(raw(
            NativeCandidateKind::File,
            NativeCandidateData::File {
                path: PathBuf::from("/home/alice/x.rs"),
                is_dir: false,
                size: Some(4096),
            },
        ));
        assert_candidate_round_trips(raw(
            NativeCandidateKind::Option,
            NativeCandidateData::Option {
                name: "wrap".into(),
                current_value: "on".into(),
                doc: "soft wrap".into(),
            },
        ));
        assert_candidate_round_trips(raw(
            NativeCandidateKind::Register,
            NativeCandidateData::Register {
                name: 'a',
                preview: "yanked".into(),
            },
        ));
        assert_candidate_round_trips(raw(
            NativeCandidateKind::Extension(7),
            NativeCandidateData::Extension {
                kind_id: 7,
                payload: vec![1, 2, 3],
            },
        ));
    }

    #[test]
    fn candidate_data_command_is_a_typed_error() {
        use lattice_grammar::source::{SourceKind, SourceLayer, SourceLocation};
        let data = NativeCandidateData::Command {
            name: "quit".into(),
            doc: "quit".into(),
            kind_label: "ex-command".into(),
            source: SourceLocation {
                layer: SourceLayer::Builtin,
                kind: SourceKind::Synthetic("<builtin>".into()),
            },
        };
        let err = data.to_wit().expect_err("Command must not cross yet");
        assert!(err.contains("Command"), "error names the culprit: {err}");
    }

    // ---- PickerAcceptOutcome ----

    fn assert_outcome_round_trips(native: NativePickerAcceptOutcome) {
        let wit = native.to_wit().expect("to_wit");
        let back = NativePickerAcceptOutcome::from_wit(wit).expect("from_wit");
        // PickerAcceptOutcome is not `PartialEq`; compare structural Debug.
        assert_eq!(format!("{native:?}"), format!("{back:?}"));
    }

    #[test]
    fn picker_accept_outcome_round_trips_every_variant() {
        for outcome in [
            NativePickerAcceptOutcome::OpenFile {
                path: PathBuf::from("/a/b.rs"),
            },
            NativePickerAcceptOutcome::SwitchBuffer { buffer_id: 3 },
            NativePickerAcceptOutcome::JumpInBuffer {
                buffer_id: 3,
                line: 10,
                col: 4,
            },
            NativePickerAcceptOutcome::JumpToMark { name: 'a' },
            NativePickerAcceptOutcome::JumpToLocation {
                path: PathBuf::from("/a/b.rs"),
                line: 1,
                col: 0,
            },
            NativePickerAcceptOutcome::InvokeCommand {
                id: "write".into(),
                args: NativeArgs::List(vec![NativeArgValue::Bool(true)]),
            },
            NativePickerAcceptOutcome::PasteRegister { name: '+' },
            NativePickerAcceptOutcome::ExpandSnippet { id: "fn".into() },
            NativePickerAcceptOutcome::OpenLspLog {
                server_id: "rust-analyzer".into(),
            },
            NativePickerAcceptOutcome::OpenLspTraceLog {
                server_id: "rust-analyzer".into(),
            },
            NativePickerAcceptOutcome::ApplyLspCodeAction {
                handle: 42,
                index: 1,
            },
            NativePickerAcceptOutcome::ApplyLspCompletion { index: 2 },
            NativePickerAcceptOutcome::ApplyColorscheme {
                name: "nord".into(),
            },
            NativePickerAcceptOutcome::NoOp,
        ] {
            assert_outcome_round_trips(outcome);
        }
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_path_is_a_typed_error_not_a_lossy_cross() {
        use std::os::unix::ffi::OsStrExt;
        let bad = PathBuf::from(std::ffi::OsStr::from_bytes(b"/inv\xff/path"));
        let outcome = NativePickerAcceptOutcome::OpenFile { path: bad };
        let err = outcome.to_wit().expect_err("non-UTF-8 path must not cross");
        assert!(err.contains("UTF-8"), "error explains why: {err}");
    }

    // ---- PS.1: guest-supplied display spans ----

    use crate::lattice::plugin_host::types::DisplaySpan as WitDisplaySpan;

    fn wit_span(start: u32, end: u32, slot: &str) -> WitDisplaySpan {
        WitDisplaySpan {
            start,
            end,
            slot: slot.to_string(),
        }
    }

    fn candidate_with_spans(display: &str, spans: Vec<WitDisplaySpan>) -> NativeRawCandidate {
        let wit = WitRawCandidate {
            text: display.to_string(),
            insert_text: None,
            display: display.to_string(),
            source: None,
            kind: WitCandidateKind::Plain,
            data: WitCandidateData::Plain,
            annotations: Vec::new(),
            display_spans: spans,
        };
        NativeRawCandidate::from_wit(wit).expect("from_wit")
    }

    /// PS.1: a guest's spans cross, and a `slot` resolves through the SAME
    /// path a `highlights.scm` capture takes — so an org-roam row is coloured
    /// by the theme's headline style rather than by a second palette.
    #[test]
    fn guest_display_spans_cross_and_resolve_their_slot() {
        let c = candidate_with_spans(
            "Reading list  (books)",
            vec![wit_span(0, 12, "text.title.1"), wit_span(12, 21, "comment")],
        );
        assert_eq!(c.display_spans.len(), 2, "got {:?}", c.display_spans);
        assert_eq!(c.display_spans[0].range, 0..12);
        assert_eq!(
            c.display_spans[0].style,
            lattice_syntax::style::name_to_style_pub("text.title.1"),
            "the slot must resolve exactly as the capture name does"
        );
        assert_eq!(
            c.display_spans[1].style,
            lattice_syntax::style::name_to_style_pub("comment")
        );
    }

    /// **A span that is not on a UTF-8 boundary is dropped, not clamped.**
    ///
    /// This is the assertion that makes the validation non-optional rather
    /// than tidy: slicing mid-codepoint panics, and a guest computing offsets
    /// in `chars` instead of bytes is an ordinary bug that must not be able to
    /// take the picker down. `Café` is 5 bytes; a char-counting guest sends 4,
    /// which lands inside the `é`.
    #[test]
    fn a_span_off_a_utf8_boundary_is_dropped() {
        let c = candidate_with_spans("Café notes", vec![wit_span(0, 4, "text.title.1")]);
        assert!(
            c.display_spans.is_empty(),
            "a mid-codepoint span must be dropped, got {:?}",
            c.display_spans
        );
        // The correct byte offset for the same text survives, so the rule is
        // "bad spans go" and not "non-ASCII goes".
        let ok = candidate_with_spans("Café notes", vec![wit_span(0, 5, "text.title.1")]);
        assert_eq!(ok.display_spans.len(), 1);
    }

    /// Out of range and inverted spans are dropped, and the row keeps its
    /// other runs — one malformed span must not cost a row its styling, and
    /// must never be clamped into a run the guest did not ask for.
    #[test]
    fn malformed_spans_are_dropped_without_taking_the_row_with_them() {
        let c = candidate_with_spans(
            "abcdef",
            vec![
                wit_span(0, 3, "keyword"),  // fine
                wit_span(4, 99, "comment"), // past the end
                wit_span(5, 2, "comment"),  // inverted
                wit_span(3, 3, "comment"),  // empty
            ],
        );
        assert_eq!(c.display_spans.len(), 1, "got {:?}", c.display_spans);
        assert_eq!(c.display_spans[0].range, 0..3);
    }

    /// An unresolvable slot KEEPS the run and renders it unstyled. The run is
    /// where the guest said it was; only its colour is unknown, and a theme
    /// element that is not registered yet is a normal transient state.
    #[test]
    fn an_unknown_slot_keeps_the_run_unstyled() {
        let c = candidate_with_spans("abcdef", vec![wit_span(0, 3, "not.a.real.capture")]);
        assert_eq!(c.display_spans.len(), 1);
        assert_eq!(
            c.display_spans[0].style,
            lattice_cells::style::Style::Default
        );
    }

    /// A source that styles nothing is byte-identical to the pre-PS.1
    /// behaviour — the overwhelmingly common case must not have changed.
    #[test]
    fn a_candidate_with_no_spans_is_unchanged() {
        let c = candidate_with_spans("plain row", Vec::new());
        assert!(c.display_spans.is_empty());
    }
}
