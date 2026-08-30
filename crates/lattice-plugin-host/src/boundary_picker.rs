//! The picker-source boundary mirrors (plugin-host.md §4.2 / §5 `picker-source`).
//!
//! Slice PH7.4a. The `picker-source` WIT interface is the plugin-facing API a
//! WASM picker source authors against (the user's "expose the api, not sources":
//! native sources stay native Rust; a plugin implements a source against these
//! types and registers through the *same* `PickerRegistry::register_generator`
//! seam a native source uses). This module round-trips that API's data types:
//!
//!   - [`Annotation`](NativeAnnotation) (+ `KeyChord`) — marginalia. The
//!     **whole** closed enum crosses (PH7.4a decision) so a plugin candidate can
//!     define + populate themed columns. `slot`/`category` are theme element
//!     KEYS resolved at paint, never baked colors, so `:colorscheme` recolors
//!     plugin marginalia live. The host lays out `AnnotationColumns` from the
//!     visible set (a render-consumed projection), so only the per-candidate
//!     annotations cross, never the column layout.
//!   - `ArgSpec` / `PickerSourceSpec` — the source's declared metadata.
//!   - `RoutingPayload` / `OpenTarget` — the per-candidate token a source emits
//!     and consumes in `accept`.
//!   - the owned `PickerContext` projection (§4.2) the host hands `init` —
//!     host→guest only, so it mirrors one-way (a `project_*` fn, the
//!     `project_buffer_snapshot` precedent), no `from_wit`.
//!
//! **The `&'static str` seam.** `PickerSourceSpec`/`ArgSpec` use `&'static str`
//! for ids/docs/prompts — native sources supply compile-time literals. A WASM
//! plugin supplies owned runtime strings, so `from_wit` **interns** them
//! ([`intern`], `Box::leak`). This is bounded by the loaded-source count (each
//! source's spec is leaked once at registration); unbounded re-registration
//! (hot reload) is a PH7.12 concern — see the slice plan.
//!
//! The active buffer's bulk rope text + syntax-highlight overlay do NOT ride the
//! context projection; they cross via the `buffer` `document` resource handle,
//! wired with the `init(ctx)` guest export at PH7.4c. A fuzzy-finder needs
//! neither.

use std::path::PathBuf;
use std::sync::Arc;

use crate::WitBoundary;
use crate::boundary::path_to_wit;

use crate::lattice::plugin_host::types::{
    ActiveBufferSnapshot as WitActiveBufferSnapshot, AiSessionPayload as WitAiSessionPayload,
    Annotation as WitAnnotation, AnnotationCustom as WitAnnotationCustom,
    AnnotationSegment as WitAnnotationSegment, AnnotationStyled as WitAnnotationStyled,
    ArgDefault as WitArgDefault, ArgKind as WitArgKind, ArgSpec as WitArgSpec,
    BufferEntry as WitBufferEntry, CommandRef as WitCommandRef, JumpTarget as WitJumpTarget,
    KeyChord as WitKeyChord, KeyKind as WitKeyKind, Location as WitLocation,
    LspInstancePayload as WitLspInstancePayload, OpenTarget as WitOpenTarget,
    PickerContext as WitPickerContext, PickerSourceSpec as WitPickerSourceSpec,
    PositionEntry as WitPositionEntry, PositionSource as WitPositionSource,
    ResolveDiffPayload as WitResolveDiffPayload, RoutingPayload as WitRoutingPayload,
    ShowMessageActionPayload as WitShowMessageActionPayload, SpecialKey as WitSpecialKey,
    SymbolLocation as WitSymbolLocation,
};

use lattice_completion::candidate::{
    Annotation as NativeAnnotation, AnnotationSegment as NativeAnnotationSegment,
};
use lattice_grammar::args::{
    ArgDefault as NativeArgDefault, ArgKind as NativeArgKind, ArgSpec as NativeArgSpec,
    ArgValue as NativeArgValue,
};
use lattice_picker::RoutingPayload as NativeRoutingPayload;
use lattice_picker::context::{
    ActiveBufferSnapshot as NativeActiveBufferSnapshot, BufferEntry as NativeBufferEntry,
    PickerContext as NativePickerContext, PositionEntry as NativePositionEntry,
    PositionSource as NativePositionSource,
};
use lattice_picker::outcome::OpenTarget as NativeOpenTarget;
use lattice_picker::source::PickerSourceSpec as NativePickerSourceSpec;
use lattice_protocol::chord::{
    KeyChord as NativeKeyChord, KeyKind as NativeKeyKind, KeyMods as NativeKeyMods,
    SpecialKey as NativeSpecialKey,
};

// PL8.F: the `intern`/`Box::leak` helper is gone. The native picker/grammar
// spec types (`PickerSourceSpec`, `ArgSpec`) now hold `Cow<'static, str>`, so a
// plugin's WIT-supplied runtime strings cross as `Cow::Owned` and free with the
// registry entry on `unregister` — no leak, still no `unsafe`.

// ---- Marginalia: KeyChord + Annotation ----

impl WitBoundary for NativeSpecialKey {
    type Wit = WitSpecialKey;

    fn to_wit(&self) -> Result<WitSpecialKey, String> {
        Ok(match self {
            NativeSpecialKey::Esc => WitSpecialKey::Esc,
            NativeSpecialKey::Enter => WitSpecialKey::Enter,
            NativeSpecialKey::Tab => WitSpecialKey::Tab,
            NativeSpecialKey::Backspace => WitSpecialKey::Backspace,
            NativeSpecialKey::Space => WitSpecialKey::Space,
            NativeSpecialKey::Up => WitSpecialKey::Up,
            NativeSpecialKey::Down => WitSpecialKey::Down,
            NativeSpecialKey::Left => WitSpecialKey::Left,
            NativeSpecialKey::Right => WitSpecialKey::Right,
            NativeSpecialKey::Home => WitSpecialKey::Home,
            NativeSpecialKey::End => WitSpecialKey::End,
            NativeSpecialKey::PageUp => WitSpecialKey::PageUp,
            NativeSpecialKey::PageDown => WitSpecialKey::PageDown,
            NativeSpecialKey::Insert => WitSpecialKey::Insert,
            NativeSpecialKey::Delete => WitSpecialKey::Delete,
            NativeSpecialKey::F(n) => WitSpecialKey::F(*n),
        })
    }

    fn from_wit(wit: WitSpecialKey) -> Result<Self, String> {
        Ok(match wit {
            WitSpecialKey::Esc => NativeSpecialKey::Esc,
            WitSpecialKey::Enter => NativeSpecialKey::Enter,
            WitSpecialKey::Tab => NativeSpecialKey::Tab,
            WitSpecialKey::Backspace => NativeSpecialKey::Backspace,
            WitSpecialKey::Space => NativeSpecialKey::Space,
            WitSpecialKey::Up => NativeSpecialKey::Up,
            WitSpecialKey::Down => NativeSpecialKey::Down,
            WitSpecialKey::Left => NativeSpecialKey::Left,
            WitSpecialKey::Right => NativeSpecialKey::Right,
            WitSpecialKey::Home => NativeSpecialKey::Home,
            WitSpecialKey::End => NativeSpecialKey::End,
            WitSpecialKey::PageUp => NativeSpecialKey::PageUp,
            WitSpecialKey::PageDown => NativeSpecialKey::PageDown,
            WitSpecialKey::Insert => NativeSpecialKey::Insert,
            WitSpecialKey::Delete => NativeSpecialKey::Delete,
            // `F(0)` is reserved/invalid (`SpecialKey::F` docs `1..=24`); reject
            // it at the boundary rather than construct an invalid chord.
            WitSpecialKey::F(0) => {
                return Err("special-key f(0) is invalid (function keys are 1..=24)".to_string());
            }
            WitSpecialKey::F(n) => NativeSpecialKey::F(n),
        })
    }
}

impl WitBoundary for NativeKeyChord {
    type Wit = WitKeyChord;

    fn to_wit(&self) -> Result<WitKeyChord, String> {
        let key = match self.key {
            NativeKeyKind::Char(c) => WitKeyKind::Char(c),
            NativeKeyKind::Special(s) => WitKeyKind::Special(s.to_wit()?),
        };
        Ok(WitKeyChord {
            key,
            mods: self.mods.0,
        })
    }

    fn from_wit(wit: WitKeyChord) -> Result<Self, String> {
        let key = match wit.key {
            WitKeyKind::Char(c) => NativeKeyKind::Char(c),
            WitKeyKind::Special(s) => NativeKeyKind::Special(NativeSpecialKey::from_wit(s)?),
        };
        Ok(NativeKeyChord {
            key,
            mods: NativeKeyMods(wit.mods),
        })
    }
}

impl WitBoundary for NativeAnnotation {
    type Wit = WitAnnotation;

    fn to_wit(&self) -> Result<WitAnnotation, String> {
        Ok(match self {
            NativeAnnotation::Kind(s) => WitAnnotation::Kind(s.to_string()),
            NativeAnnotation::DocSnippet(s) => WitAnnotation::DocSnippet(s.to_string()),
            NativeAnnotation::Keybinding(chords) => {
                WitAnnotation::Keybinding(chords.iter().map(WitBoundary::to_wit).collect::<Result<
                    Vec<_>,
                    String,
                >>(
                )?)
            }
            NativeAnnotation::Source(s) => WitAnnotation::Source(s.to_string()),
            NativeAnnotation::Custom { text, slot } => WitAnnotation::Custom(WitAnnotationCustom {
                text: text.to_string(),
                slot: slot.to_string(),
            }),
            NativeAnnotation::Styled { category, segments } => {
                WitAnnotation::Styled(WitAnnotationStyled {
                    category: category.to_string(),
                    segments: segments
                        .iter()
                        .map(|seg| WitAnnotationSegment {
                            text: seg.text.to_string(),
                            slot: seg.slot.to_string(),
                        })
                        .collect(),
                })
            }
        })
    }

    fn from_wit(wit: WitAnnotation) -> Result<Self, String> {
        Ok(match wit {
            WitAnnotation::Kind(s) => NativeAnnotation::Kind(Arc::from(s)),
            WitAnnotation::DocSnippet(s) => NativeAnnotation::DocSnippet(Arc::from(s)),
            WitAnnotation::Keybinding(chords) => NativeAnnotation::Keybinding(
                chords
                    .into_iter()
                    .map(NativeKeyChord::from_wit)
                    .collect::<Result<Vec<_>, String>>()?,
            ),
            WitAnnotation::Source(s) => NativeAnnotation::Source(Arc::from(s)),
            WitAnnotation::Custom(c) => NativeAnnotation::Custom {
                text: Arc::from(c.text),
                slot: Arc::from(c.slot),
            },
            WitAnnotation::Styled(st) => NativeAnnotation::Styled {
                category: Arc::from(st.category),
                segments: st
                    .segments
                    .into_iter()
                    .map(|seg| NativeAnnotationSegment {
                        text: Arc::from(seg.text),
                        slot: Arc::from(seg.slot),
                    })
                    .collect(),
            },
        })
    }
}

// ---- Source spec: ArgKind / ArgDefault / ArgSpec / PickerSourceSpec ----

impl WitBoundary for NativeArgKind {
    type Wit = WitArgKind;

    fn to_wit(&self) -> Result<WitArgKind, String> {
        Ok(match self {
            NativeArgKind::String => WitArgKind::String,
            NativeArgKind::Char => WitArgKind::Char,
            NativeArgKind::Bool => WitArgKind::Bool,
            NativeArgKind::Int => WitArgKind::Int,
            NativeArgKind::Pattern => WitArgKind::Pattern,
            NativeArgKind::Chord => WitArgKind::Chord,
            NativeArgKind::Body => WitArgKind::Body,
            NativeArgKind::Raw => WitArgKind::Raw,
        })
    }

    fn from_wit(wit: WitArgKind) -> Result<Self, String> {
        Ok(match wit {
            WitArgKind::String => NativeArgKind::String,
            WitArgKind::Char => NativeArgKind::Char,
            WitArgKind::Bool => NativeArgKind::Bool,
            WitArgKind::Int => NativeArgKind::Int,
            WitArgKind::Pattern => NativeArgKind::Pattern,
            WitArgKind::Chord => NativeArgKind::Chord,
            WitArgKind::Body => NativeArgKind::Body,
            WitArgKind::Raw => NativeArgKind::Raw,
        })
    }
}

impl WitBoundary for NativeArgDefault {
    type Wit = WitArgDefault;

    fn to_wit(&self) -> Result<WitArgDefault, String> {
        Ok(match self {
            NativeArgDefault::Required => WitArgDefault::Required,
            NativeArgDefault::None => WitArgDefault::None,
            NativeArgDefault::Literal(v) => WitArgDefault::Literal(v.to_wit()?),
            NativeArgDefault::UseSelection => WitArgDefault::UseSelection,
            NativeArgDefault::UseCursorWord => WitArgDefault::UseCursorWord,
            NativeArgDefault::UseLastResponse => WitArgDefault::UseLastResponse,
        })
    }

    fn from_wit(wit: WitArgDefault) -> Result<Self, String> {
        Ok(match wit {
            WitArgDefault::Required => NativeArgDefault::Required,
            WitArgDefault::None => NativeArgDefault::None,
            WitArgDefault::Literal(v) => NativeArgDefault::Literal(NativeArgValue::from_wit(v)?),
            WitArgDefault::UseSelection => NativeArgDefault::UseSelection,
            WitArgDefault::UseCursorWord => NativeArgDefault::UseCursorWord,
            WitArgDefault::UseLastResponse => NativeArgDefault::UseLastResponse,
        })
    }
}

impl WitBoundary for NativeArgSpec {
    type Wit = WitArgSpec;

    fn to_wit(&self) -> Result<WitArgSpec, String> {
        Ok(WitArgSpec {
            name: self.name.to_string(),
            kind: self.kind.to_wit()?,
            doc: self.doc.to_string(),
            prompt: self.prompt.to_string(),
            default: self.default.to_wit()?,
            completion: self.completion.as_deref().map(str::to_string),
            picker: self.picker.as_deref().map(str::to_string),
        })
    }

    fn from_wit(wit: WitArgSpec) -> Result<Self, String> {
        // PL8.F: the plugin's runtime strings become `Cow::Owned` on the native
        // `ArgSpec` — no `Box::leak`. They free with the command entry on
        // `unregister_plugin`.
        Ok(NativeArgSpec {
            name: wit.name.into(),
            kind: NativeArgKind::from_wit(wit.kind)?,
            doc: wit.doc.into(),
            prompt: wit.prompt.into(),
            default: NativeArgDefault::from_wit(wit.default)?,
            completion: wit.completion.map(Into::into),
            picker: wit.picker.map(Into::into),
        })
    }
}

impl WitBoundary for NativePickerSourceSpec {
    type Wit = WitPickerSourceSpec;

    fn to_wit(&self) -> Result<WitPickerSourceSpec, String> {
        Ok(WitPickerSourceSpec {
            id: self.id.to_string(),
            doc: self.doc.to_string(),
            args_schema: self
                .args_schema
                .iter()
                .map(WitBoundary::to_wit)
                .collect::<Result<Vec<_>, String>>()?,
            args_hint: self.args_hint.to_string(),
            live: self.live,
            create_label: self.create_label.as_ref().map(|l| l.to_string()),
        })
    }

    fn from_wit(wit: WitPickerSourceSpec) -> Result<Self, String> {
        // PL8.F: `Cow::Owned` — the plugin's id/doc/args_hint free on
        // `PickerRegistry::unregister`, no `Box::leak`.
        Ok(NativePickerSourceSpec {
            id: wit.id.into(),
            doc: wit.doc.into(),
            args_schema: wit
                .args_schema
                .into_iter()
                .map(NativeArgSpec::from_wit)
                .collect::<Result<Vec<_>, String>>()?,
            args_hint: wit.args_hint.into(),
            live: wit.live,
            // OR.5: `Cow::Owned`, like the id/doc/args_hint above — a plugin's
            // label frees on `PickerRegistry::unregister`.
            create_label: wit.create_label.map(Into::into),
        })
    }
}

// ---- OpenTarget + RoutingPayload ----

impl WitBoundary for NativeOpenTarget {
    type Wit = WitOpenTarget;

    fn to_wit(&self) -> Result<WitOpenTarget, String> {
        Ok(match self {
            NativeOpenTarget::Default => WitOpenTarget::Default,
            NativeOpenTarget::Split => WitOpenTarget::Split,
            NativeOpenTarget::VSplit => WitOpenTarget::Vsplit,
            NativeOpenTarget::Tab => WitOpenTarget::Tab,
        })
    }

    fn from_wit(wit: WitOpenTarget) -> Result<Self, String> {
        Ok(match wit {
            WitOpenTarget::Default => NativeOpenTarget::Default,
            WitOpenTarget::Split => NativeOpenTarget::Split,
            WitOpenTarget::Vsplit => NativeOpenTarget::VSplit,
            WitOpenTarget::Tab => NativeOpenTarget::Tab,
        })
    }
}

impl WitBoundary for NativeRoutingPayload {
    type Wit = WitRoutingPayload;

    fn to_wit(&self) -> Result<WitRoutingPayload, String> {
        Ok(match self {
            NativeRoutingPayload::Buffer { id } => WitRoutingPayload::Buffer(*id),
            NativeRoutingPayload::PaneHistoryEntry { index } => {
                WitRoutingPayload::PaneHistoryEntry(*index)
            }
            NativeRoutingPayload::ResolveDiff { primary, accept } => {
                WitRoutingPayload::ResolveDiff(WitResolveDiffPayload {
                    primary: *primary,
                    accept: *accept,
                })
            }
            NativeRoutingPayload::LspInstance {
                server_id,
                workspace,
            } => WitRoutingPayload::LspInstance(WitLspInstancePayload {
                server_id: server_id.clone(),
                workspace: path_to_wit(workspace)?,
            }),
            NativeRoutingPayload::LspLocation { path, line, col } => {
                WitRoutingPayload::LspLocation(WitLocation {
                    path: path_to_wit(path)?,
                    line: *line,
                    col: *col,
                })
            }
            NativeRoutingPayload::LspCompletion { index } => {
                WitRoutingPayload::LspCompletion(*index)
            }
            NativeRoutingPayload::LspCodeAction { index } => {
                WitRoutingPayload::LspCodeAction(*index)
            }
            NativeRoutingPayload::OpenFile { path } => {
                WitRoutingPayload::OpenFile(path_to_wit(path)?)
            }
            NativeRoutingPayload::JumpInBuffer {
                buffer_id,
                line,
                col,
            } => WitRoutingPayload::JumpInBuffer(WitJumpTarget {
                buffer_id: *buffer_id,
                line: *line,
                col: *col,
            }),
            NativeRoutingPayload::InvokeCommand { id, args } => {
                WitRoutingPayload::InvokeCommand(WitCommandRef {
                    id: id.clone(),
                    args: args.to_wit()?,
                })
            }
            NativeRoutingPayload::PasteRegister { name } => WitRoutingPayload::PasteRegister(*name),
            NativeRoutingPayload::JumpToMark { name } => WitRoutingPayload::JumpToMark(*name),
            NativeRoutingPayload::ExpandSnippet { id } => {
                WitRoutingPayload::ExpandSnippet(id.clone())
            }
            NativeRoutingPayload::AcceptShowMessageAction {
                request_id,
                action_index,
            } => WitRoutingPayload::AcceptShowMessageAction(WitShowMessageActionPayload {
                request_id: *request_id,
                action_index: *action_index,
            }),
            NativeRoutingPayload::LspCodeLens { index } => WitRoutingPayload::LspCodeLens(*index),
            NativeRoutingPayload::ColorPresentation { index } => {
                WitRoutingPayload::ColorPresentation(*index)
            }
            NativeRoutingPayload::Colorscheme { name } => {
                WitRoutingPayload::Colorscheme(name.clone())
            }
            NativeRoutingPayload::AiSession { provider, index } => {
                WitRoutingPayload::AiSession(WitAiSessionPayload {
                    provider: provider.clone(),
                    index: *index,
                })
            }
            // MB.3: `LoadCommandLine` is the native `history` picker's
            // per-candidate token — host-internal, not part of the plugin
            // WIT surface (a plugin picker source cannot seed the `:` line).
            // It never crosses the boundary.
            NativeRoutingPayload::LoadCommandLine { .. } => {
                return Err(
                    "load-command-line is a host-internal picker routing payload, not representable over WIT"
                        .into(),
                );
            }
            // MB.5: `LoadSearchLine` — same as LoadCommandLine above,
            // host-internal search-history picker routing.
            NativeRoutingPayload::LoadSearchLine { .. } => {
                return Err(
                    "load-search-line is a host-internal picker routing payload, not representable over WIT"
                        .into(),
                );
            }
            // magit's branch-create wizard — host-internal, same shape
            // as LoadCommandLine/LoadSearchLine above.
            NativeRoutingPayload::BranchBase { .. } => {
                return Err(
                    "branch-base is a host-internal picker routing payload, not representable over WIT"
                        .into(),
                );
            }
            // MG.53.e: pairs with `supply-value` in `boundary.rs` — the
            // thing waiting for the value is host state a plugin cannot
            // address.
            NativeRoutingPayload::SuppliedValue { .. } => {
                return Err(
                    "supplied-value is a host-internal picker routing payload, not representable over WIT"
                        .into(),
                );
            }
            // OR.6: a place in a file. Crosses like `lsp-location` does.
            NativeRoutingPayload::FileLocation { path, line, col } => {
                WitRoutingPayload::FileLocation(crate::lattice::plugin_host::types::Location {
                    path: crate::boundary::path_to_wit(path)?,
                    line: *line,
                    col: *col,
                })
            }
            // OR.5: the create row's query, crossing VERBATIM. This one DOES
            // reach a plugin — it is the whole point of the slice, since the
            // source that declared `create_label` is the one that has to decide
            // what creation means.
            NativeRoutingPayload::Create { query } => WitRoutingPayload::Create(query.clone()),
        })
    }

    fn from_wit(wit: WitRoutingPayload) -> Result<Self, String> {
        Ok(match wit {
            WitRoutingPayload::Buffer(id) => NativeRoutingPayload::Buffer { id },
            // OR.6: a place in a file, back from the guest.
            WitRoutingPayload::FileLocation(loc) => NativeRoutingPayload::FileLocation {
                path: std::path::PathBuf::from(loc.path),
                line: loc.line,
                col: loc.col,
            },
            // OR.5. A guest never *emits* one of these — the picker synthesises
            // the row — but the mirror is symmetric so a guest that echoes a
            // routing token back round-trips rather than erroring.
            WitRoutingPayload::Create(query) => NativeRoutingPayload::Create { query },
            WitRoutingPayload::PaneHistoryEntry(index) => {
                NativeRoutingPayload::PaneHistoryEntry { index }
            }
            WitRoutingPayload::ResolveDiff(p) => NativeRoutingPayload::ResolveDiff {
                primary: p.primary,
                accept: p.accept,
            },
            WitRoutingPayload::LspInstance(p) => NativeRoutingPayload::LspInstance {
                server_id: p.server_id,
                workspace: PathBuf::from(p.workspace),
            },
            WitRoutingPayload::LspLocation(l) => NativeRoutingPayload::LspLocation {
                path: PathBuf::from(l.path),
                line: l.line,
                col: l.col,
            },
            WitRoutingPayload::LspCompletion(index) => {
                NativeRoutingPayload::LspCompletion { index }
            }
            WitRoutingPayload::LspCodeAction(index) => {
                NativeRoutingPayload::LspCodeAction { index }
            }
            WitRoutingPayload::OpenFile(path) => NativeRoutingPayload::OpenFile {
                path: PathBuf::from(path),
            },
            WitRoutingPayload::JumpInBuffer(t) => NativeRoutingPayload::JumpInBuffer {
                buffer_id: t.buffer_id,
                line: t.line,
                col: t.col,
            },
            WitRoutingPayload::InvokeCommand(c) => NativeRoutingPayload::InvokeCommand {
                id: c.id,
                args: lattice_grammar::args::Args::from_wit(c.args)?,
            },
            WitRoutingPayload::PasteRegister(name) => NativeRoutingPayload::PasteRegister { name },
            WitRoutingPayload::JumpToMark(name) => NativeRoutingPayload::JumpToMark { name },
            WitRoutingPayload::ExpandSnippet(id) => NativeRoutingPayload::ExpandSnippet { id },
            WitRoutingPayload::AcceptShowMessageAction(p) => {
                NativeRoutingPayload::AcceptShowMessageAction {
                    request_id: p.request_id,
                    action_index: p.action_index,
                }
            }
            WitRoutingPayload::LspCodeLens(index) => NativeRoutingPayload::LspCodeLens { index },
            WitRoutingPayload::ColorPresentation(index) => {
                NativeRoutingPayload::ColorPresentation { index }
            }
            WitRoutingPayload::Colorscheme(name) => NativeRoutingPayload::Colorscheme { name },
            WitRoutingPayload::AiSession(p) => NativeRoutingPayload::AiSession {
                provider: p.provider,
                index: p.index,
            },
        })
    }
}

// ---- The owned `PickerContext` projection (§4.2, host→guest only) ----

fn project_position_source(src: &NativePositionSource) -> WitPositionSource {
    match src {
        NativePositionSource::AutoJump => WitPositionSource::AutoJump,
        NativePositionSource::ExplicitMark => WitPositionSource::ExplicitMark,
        NativePositionSource::PluginPush => WitPositionSource::PluginPush,
        NativePositionSource::NamedMark(c) => WitPositionSource::NamedMark(*c),
    }
}

fn project_position_entry(e: &NativePositionEntry) -> WitPositionEntry {
    WitPositionEntry {
        buffer_id: e.buffer_id,
        line: e.line,
        col: e.col,
        source: project_position_source(&e.source),
    }
}

fn project_buffer_entry(e: &NativeBufferEntry) -> Result<WitBufferEntry, String> {
    Ok(WitBufferEntry {
        id: e.id,
        kind_label: e.kind_label.clone(),
        path: e.path.as_deref().map(path_to_wit).transpose()?,
        title: e.title.clone(),
        dirty: e.dirty,
    })
}

fn project_active_buffer_snapshot(
    ab: &NativeActiveBufferSnapshot<'_>,
) -> Result<WitActiveBufferSnapshot, String> {
    let selection = match ab.selection {
        Some((a, b)) => Some((a.to_wit()?, b.to_wit()?)),
        None => None,
    };
    Ok(WitActiveBufferSnapshot {
        buffer_id: ab.buffer_id,
        path: ab.path.map(path_to_wit).transpose()?,
        language: ab.language.map(str::to_string),
        cursor: ab.cursor.to_wit()?,
        selection,
        syntax_symbols: ab
            .syntax_symbols
            .iter()
            .map(|(name, line, col)| WitSymbolLocation {
                name: name.clone(),
                line: *line,
                col: *col,
            })
            .collect(),
    })
}

/// Project a live [`PickerContext`](NativePickerContext) into its owned WIT
/// mirror (§4.2). Host→guest only: the host builds this at `init` time; the
/// guest never sends a context back. A non-UTF-8 path anywhere in the context
/// is a typed error (§4.4), never a lossy encoding.
pub fn project_picker_context(ctx: &NativePickerContext<'_>) -> Result<WitPickerContext, String> {
    Ok(WitPickerContext {
        active_buffer: project_active_buffer_snapshot(&ctx.active_buffer)?,
        workspace_root: path_to_wit(&ctx.workspace_root)?,
        recent_files: ctx
            .recent_files
            .iter()
            .map(|p| path_to_wit(p))
            .collect::<Result<Vec<_>, String>>()?,
        position_history: ctx
            .position_history
            .iter()
            .map(project_position_entry)
            .collect(),
        buffers: ctx
            .buffers
            .iter()
            .map(project_buffer_entry)
            .collect::<Result<Vec<_>, String>>()?,
        marks: ctx
            .marks
            .iter()
            .map(|(c, pos)| Ok((*c, pos.to_wit()?)))
            .collect::<Result<Vec<_>, String>>()?,
        registers: ctx.registers.clone(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use lattice_completion::candidate::AnnotationSegment;

    // ---- KeyChord + Annotation ----

    fn assert_chord_round_trips(native: NativeKeyChord) {
        let back = NativeKeyChord::from_wit(native.to_wit().unwrap()).unwrap();
        assert_eq!(native, back);
    }

    #[test]
    fn key_chord_round_trips_char_special_and_mods() {
        assert_chord_round_trips(NativeKeyChord {
            key: NativeKeyKind::Char('a'),
            mods: NativeKeyMods::NONE,
        });
        assert_chord_round_trips(NativeKeyChord {
            key: NativeKeyKind::Char('x'),
            mods: NativeKeyMods::CTRL,
        });
        assert_chord_round_trips(NativeKeyChord {
            key: NativeKeyKind::Special(NativeSpecialKey::Tab),
            mods: NativeKeyMods(NativeKeyMods::CTRL.0 | NativeKeyMods::SHIFT.0),
        });
        assert_chord_round_trips(NativeKeyChord {
            key: NativeKeyKind::Special(NativeSpecialKey::F(12)),
            mods: NativeKeyMods::ALT,
        });
    }

    #[test]
    fn special_key_f0_is_a_typed_error() {
        let err = NativeSpecialKey::from_wit(WitSpecialKey::F(0)).expect_err("f(0) must reject");
        assert!(err.contains("f(0)"), "error names the culprit: {err}");
    }

    fn assert_annotation_round_trips(native: NativeAnnotation) {
        let back = NativeAnnotation::from_wit(native.to_wit().unwrap()).unwrap();
        assert_eq!(native, back);
    }

    #[test]
    fn annotation_round_trips_every_variant() {
        assert_annotation_round_trips(NativeAnnotation::Kind(Arc::from("(file)")));
        assert_annotation_round_trips(NativeAnnotation::DocSnippet(Arc::from("opens a file")));
        assert_annotation_round_trips(NativeAnnotation::Keybinding(vec![
            NativeKeyChord {
                key: NativeKeyKind::Char('g'),
                mods: NativeKeyMods::NONE,
            },
            NativeKeyChord {
                key: NativeKeyKind::Special(NativeSpecialKey::Esc),
                mods: NativeKeyMods::CTRL,
            },
        ]));
        assert_annotation_round_trips(NativeAnnotation::Source(Arc::from("lsp")));
        assert_annotation_round_trips(NativeAnnotation::Custom {
            text: Arc::from("42K"),
            slot: Arc::from("annotation_size"),
        });
        // The marginalia column a plugin file source would emit: a permission
        // string split into per-bit-class segments, each its own theme slot.
        assert_annotation_round_trips(NativeAnnotation::Styled {
            category: Arc::from("perms"),
            segments: vec![
                AnnotationSegment {
                    text: Arc::from("d"),
                    slot: Arc::from("perm_dir"),
                },
                AnnotationSegment {
                    text: Arc::from("rwx"),
                    slot: Arc::from("perm_owner"),
                },
            ],
        });
    }

    // ---- ArgSpec / PickerSourceSpec (leak-tolerant field compare) ----

    #[test]
    fn arg_spec_round_trips_through_intern() {
        let native = NativeArgSpec {
            name: "root".into(),
            kind: NativeArgKind::String,
            doc: "directory to walk".into(),
            prompt: "root:".into(),
            default: NativeArgDefault::Literal(NativeArgValue::String("/tmp".into())),
            completion: Some("gen:files".into()),
            picker: None,
        };
        let back = NativeArgSpec::from_wit(native.to_wit().unwrap()).unwrap();
        assert_eq!(back.name, "root");
        assert_eq!(back.doc, "directory to walk");
        assert_eq!(back.prompt, "root:");
        assert_eq!(back.completion.as_deref(), Some("gen:files"));
        assert!(matches!(back.kind, NativeArgKind::String));
        assert!(matches!(
            back.default,
            NativeArgDefault::Literal(NativeArgValue::String(ref s)) if s == "/tmp"
        ));
    }

    #[test]
    fn picker_source_spec_round_trips() {
        let native = NativePickerSourceSpec {
            id: "files".into(),
            doc: "file picker".into(),
            args_schema: vec![NativeArgSpec {
                name: "root".into(),
                kind: NativeArgKind::String,
                doc: "dir".into(),
                prompt: "root:".into(),
                default: NativeArgDefault::None,
                completion: Some("gen:files".into()),
                picker: None,
            }],
            args_hint: "[root]".into(),
            live: false,
            // OR.5: a real label, so the round trip proves the field crosses
            // rather than proving `None == None`.
            create_label: Some("Create note: %s".into()),
        };
        let back = NativePickerSourceSpec::from_wit(native.to_wit().unwrap()).unwrap();
        assert_eq!(back.id, "files");
        assert_eq!(back.doc, "file picker");
        assert_eq!(back.args_hint, "[root]");
        assert!(!back.live);
        assert_eq!(back.create_label.as_deref(), Some("Create note: %s"));
        assert_eq!(back.args_schema.len(), 1);
        assert_eq!(back.args_schema[0].name, "root");
    }

    // ---- OpenTarget + RoutingPayload ----

    #[test]
    fn open_target_round_trips_every_variant() {
        for t in [
            NativeOpenTarget::Default,
            NativeOpenTarget::Split,
            NativeOpenTarget::VSplit,
            NativeOpenTarget::Tab,
        ] {
            let back = NativeOpenTarget::from_wit(t.to_wit().unwrap()).unwrap();
            assert_eq!(t, back);
        }
    }

    /// `RoutingPayload` carries `Args` (no `PartialEq`), so assert via the round-
    /// tripped Debug shape — an equality proxy that still catches field drift.
    fn assert_routing_round_trips(native: NativeRoutingPayload) {
        let dbg = format!("{native:?}");
        let back = NativeRoutingPayload::from_wit(native.to_wit().unwrap()).unwrap();
        assert_eq!(dbg, format!("{back:?}"));
    }

    #[test]
    fn routing_payload_round_trips_representative_variants() {
        assert_routing_round_trips(NativeRoutingPayload::Buffer { id: 7 });
        assert_routing_round_trips(NativeRoutingPayload::ResolveDiff {
            primary: 3,
            accept: true,
        });
        assert_routing_round_trips(NativeRoutingPayload::LspInstance {
            server_id: "rust-analyzer".into(),
            workspace: PathBuf::from("/home/x/proj"),
        });
        assert_routing_round_trips(NativeRoutingPayload::LspLocation {
            path: PathBuf::from("/a/b.rs"),
            line: 12,
            col: 4,
        });
        assert_routing_round_trips(NativeRoutingPayload::OpenFile {
            path: PathBuf::from("/tmp/foo.rs"),
        });
        assert_routing_round_trips(NativeRoutingPayload::JumpInBuffer {
            buffer_id: 2,
            line: 40,
            col: 1,
        });
        assert_routing_round_trips(NativeRoutingPayload::InvokeCommand {
            id: "write".into(),
            args: lattice_grammar::args::Args::None,
        });
        assert_routing_round_trips(NativeRoutingPayload::PasteRegister { name: 'a' });
        assert_routing_round_trips(NativeRoutingPayload::JumpToMark { name: 'z' });
        assert_routing_round_trips(NativeRoutingPayload::ExpandSnippet { id: "fn".into() });
        assert_routing_round_trips(NativeRoutingPayload::AcceptShowMessageAction {
            request_id: 5,
            action_index: 1,
        });
        assert_routing_round_trips(NativeRoutingPayload::LspCodeLens { index: 9 });
        assert_routing_round_trips(NativeRoutingPayload::ColorPresentation { index: 0 });
        assert_routing_round_trips(NativeRoutingPayload::Colorscheme {
            name: "nord".into(),
        });
    }

    #[test]
    #[cfg(unix)]
    fn routing_payload_non_utf8_path_is_a_typed_error() {
        use std::os::unix::ffi::OsStrExt;
        let bad = std::ffi::OsStr::from_bytes(&[0xff, 0xfe]);
        let native = NativeRoutingPayload::OpenFile {
            path: PathBuf::from(bad),
        };
        let err = native.to_wit().expect_err("non-utf8 path must reject");
        assert!(err.contains("UTF-8"), "error explains the failure: {err}");
    }
}
