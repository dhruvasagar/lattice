//! Unified source registration — slice `3c.unify.picker-generator-trait-unify`
//! (7a).
//!
//! Defines the shapes that picker, cmdline-completion, and
//! (post-WASM-host) plugins all use to register a candidate
//! source. The design comes out of the LSP cross-check in
//! `docs/dev/architecture/completion-pipeline-unification.md`
//! § "LSP cross-check: the design that survives".
//!
//! ## Status
//!
//! Slice 7a lands the **shapes**: traits, enums, the
//! `SourceRegistration` bundle. No first-party source migration
//! yet (that's 7b); no registry integration (that's 7c); no
//! `:picker <name>` cutover (that's 7d). This file exists so
//! 7b can migrate against a stable target.
//!
//! ## End-state architecture
//!
//! ```text
//! lattice-completion::
//!     CandidateGenerator      // existing — pull-based source
//!     AcceptHandler           // NEW — stateless accept dispatch
//!     AcceptAction            // NEW — what the host should do on accept
//!     CandidateSourceKind     // NEW — pull (Generator) vs push (PreSupplied)
//!     SourceRegistration      // NEW — the bundle: generator/accept/spec/overrides
//!     SourceSpec              // NEW — metadata (id, doc, args-schema, live flag)
//!
//! lattice-picker::
//!     PickerSourceGenerator   // KEEPS WORKING during migration
//!     // First-party sources will impl CandidateGenerator + AcceptHandler;
//!     // PickerSourceGenerator becomes a deprecated thin adapter,
//!     // then retires once 7d cuts the registry over.
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use crate::candidate::RawCandidate;
use crate::registry::{AnnotatorId, MatcherId, RankerId};
use crate::traits::CandidateGenerator;

/// Metadata for a source. Returned by the source-registration's
/// `spec` field; used for `:describe-picker` / `:picker <Tab>` /
/// `:apropos` introspection.
#[derive(Debug, Clone)]
pub struct SourceSpec {
    /// Stable id — `:picker <id>` invokes the source. Stable
    /// across versions; renames break user keybindings.
    pub id: String,
    /// User-facing one-line summary. Shown in `:picker <Tab>`
    /// completion and `:describe-picker` rows.
    pub doc: String,
    /// Whether the source accepts positional args via
    /// `:picker <id> <args...>`. `None` ⇒ no args; `Some(schema)`
    /// declares positional shape. v1 keeps the schema opaque
    /// (description string); a future slice can grow this to a
    /// typed validator.
    pub args_schema: Option<ArgsSchema>,
    /// `true` ⇒ the source's results refresh on every query
    /// change (e.g. `:picker grep` where the external process
    /// IS the filter). Picker bypasses fuzzy-refilter in live
    /// mode.
    pub live: bool,
}

/// Opaque positional-arg schema. v1 placeholder — description
/// string. A future slice can grow this to a typed validator
/// (number of required args, whether trailing args are allowed,
/// etc.).
#[derive(Debug, Clone)]
pub struct ArgsSchema {
    pub description: String,
}

/// How candidates flow into the pipeline. Picker calls have two
/// shapes (per the LSP cross-check): synchronous enumeration
/// (`gen:files` walks the FS per filter) vs. push-from-async
/// (LSP picker host-builds rows from an async response before
/// opening the picker).
#[derive(Clone)]
pub enum CandidateSourceKind {
    /// Pull-based — `generate(ctx)` runs per `Pipeline::run`.
    /// First-party uses: Files, Buffers, Commands, Lines,
    /// Jumps, Marks, Registers, Outline, RecentFiles. Plugin
    /// uses: anything synchronously enumerable.
    Generator(Arc<dyn CandidateGenerator>),

    /// Push-based — caller supplies the candidate set up front
    /// (typically from an async response). The pipeline treats
    /// the supplied Vec as a fixed input and runs only the
    /// match + rank + annotate stages.
    /// First-party uses: every LSP picker (references /
    /// definitions / completion / code-actions / code-lens /
    /// color-presentation / instances / show-message-request).
    /// Plugin uses: anything async / network-driven.
    PreSupplied(Arc<Vec<RawCandidate>>),
}

impl std::fmt::Debug for CandidateSourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Generator(_) => write!(f, "Generator(<dyn CandidateGenerator>)"),
            Self::PreSupplied(rows) => write!(f, "PreSupplied({} candidates)", rows.len()),
        }
    }
}

/// Translate a chosen candidate into a typed action the host
/// dispatches. Stateless: the handler reads the candidate, the
/// host runs the action. State lookup for indexed variants
/// (LSP completion / code-actions / etc.) happens at dispatch
/// time inside the host, where `Editor::pending_*_items` is in
/// scope.
///
/// See the design doc § "Implication: AcceptHandler is
/// stateless" for the cross-check that validated this shape.
pub trait AcceptHandler: Send + Sync {
    fn accept(&self, candidate: &RawCandidate) -> AcceptAction;
}

/// What the host should do when the user accepts a candidate.
/// Cleanup-and-rename of today's `lattice_picker::RoutingPayload`
/// plus new variants for cmdline-completion (`InsertText`) and
/// plugin extensibility (`Custom`).
///
/// **Stateless variants** carry the full payload — no host
/// lookup required. The accept handler returns one of these
/// directly; the host's dispatch arm reads the payload and
/// runs the action.
///
/// **Stateful variants** carry an `AcceptToken` (opaque marker
/// the host uses to find the right `pending_*` table) plus an
/// index. The host's dispatch arm resolves the token to a
/// pending-table reference, looks up the item by index,
/// applies it.
///
/// **Custom** is the plugin escape hatch — the plugin's accept
/// handler returns `Custom(Box::new(MyType { ... }))`; the
/// plugin's matching dispatch handler downcasts to `MyType`
/// and applies its own logic.
#[derive(Debug)]
pub enum AcceptAction {
    // -------- Stateless: candidate carries the full payload --------
    /// Hand `path` to `App::do_edit(Some(path), false)`.
    /// First-party: `:picker files`, `:picker recent`.
    OpenFile { path: PathBuf },

    /// Activate buffer `id` in the current pane.
    /// First-party: `:picker buffers`.
    SwitchBuffer { id: lattice_core::BufferId },

    /// Jump to `(path, line, col)` via `App::jump_to_file_line_col`.
    /// First-party: LSP references / definitions / type-defs /
    /// implementations / declaration / diagnostics.
    JumpToFileLocation {
        path: PathBuf,
        line: u32,
        col: u32,
    },

    /// Jump to `(line, col)` in an already-open buffer.
    /// First-party: `:picker lines`, `:picker jumps`.
    JumpInBuffer {
        buffer_id: lattice_core::BufferId,
        line: u32,
        col: u32,
    },

    /// Invoke ex-command `id` with `args`.
    /// First-party: `:picker commands` (the command palette).
    InvokeCommand {
        id: String,
        args: lattice_grammar::args::Args,
    },

    /// Paste named register `name` at the cursor.
    /// First-party: `:picker registers`.
    PasteRegister { name: char },

    /// Jump to mark `name` via `App::do_jump_mark`.
    /// First-party: `:picker marks`.
    JumpToMark { name: char },

    /// Expand snippet `id` at the cursor.
    /// First-party: `:picker snippets`.
    ExpandSnippet { id: String },

    /// Open `*lsp:<server_id>*` (the per-server log buffer) in
    /// the current pane.
    /// First-party: `:lsp-log` / `:lsp-server-log` picker.
    OpenLspLog {
        server_id: String,
        workspace: PathBuf,
    },

    /// Open `*lsp:<server_id>:trace*` (the trace ring view)
    /// without flipping the trace toggle.
    /// First-party: `:lsp-trace-log` picker.
    OpenLspTraceLog {
        server_id: String,
        workspace: PathBuf,
    },

    // -------- Stateful: host resolves by (token, index) --------
    /// Accept an LSP completion item from a pending request.
    /// First-party: `:complete`.
    AcceptIndexedCompletion {
        token: AcceptToken,
        index: u32,
    },

    /// Accept an LSP code-action item.
    /// First-party: `:code-actions` / `gA`.
    AcceptIndexedCodeAction {
        token: AcceptToken,
        index: u32,
    },

    /// Accept an LSP code-lens item.
    /// First-party: `:lsp-code-lens`.
    AcceptIndexedCodeLens {
        token: AcceptToken,
        index: u32,
    },

    /// Accept a color-presentation item.
    /// First-party: `:lsp-color-presentation`.
    AcceptColorPresentation {
        token: AcceptToken,
        index: u32,
    },

    /// Reply to a server-initiated `window/showMessageRequest`
    /// with the selected action index. Host looks up the
    /// pending oneshot by `request_id` on `server_id`.
    AcceptShowMessageAction {
        request_id: u32,
        server_id: String,
        index: u32,
    },

    // -------- Cmdline-completion --------
    /// Replace `cmdline[replace_start..]` with `text`. Used by
    /// the cmdline-completion popup when the user presses
    /// `<CR>` on a candidate. Currently handled inline in
    /// `Editor::do_command_line_accept_completion` — this
    /// variant moves that logic into the unified action enum so
    /// plugin-registered cmdline sources can use the same
    /// dispatch path.
    InsertText {
        text: String,
        replace_start: usize,
    },

    // -------- Plugin extension --------
    /// Plugin-defined opaque payload. The plugin's accept
    /// handler returns `Custom(...)`; the plugin's matching
    /// dispatch handler (registered alongside the source)
    /// downcasts to its known type and applies its own logic.
    Custom(CustomAcceptPayload),
}

/// Opaque payload for `AcceptAction::Custom`. Wrapper exists
/// to give `AcceptAction` a `Debug` impl that doesn't try to
/// format `dyn Any`. Not `Clone` — plugins that need clonable
/// payloads should wrap their type in `Arc<MyType>` and store
/// `Arc<MyType>` inside the Box.
pub struct CustomAcceptPayload(pub Box<dyn std::any::Any + Send + Sync>);

impl std::fmt::Debug for CustomAcceptPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CustomAcceptPayload(<opaque>)")
    }
}

/// Opaque marker the host uses to find the right `pending_*`
/// table for a stateful AcceptAction. v1: a `u64` that the host
/// generates per-LSP-request; the LSP cache keys on the same
/// token. Plugins use the same scheme for their own pending
/// tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AcceptToken(pub u64);

impl AcceptToken {
    pub fn new(value: u64) -> Self {
        Self(value)
    }
}

/// The substrate: bundle of every input the pipeline needs to
/// produce + dispatch a source's candidates.
///
/// Lives in `lattice-completion` (the abstraction layer); the
/// host (`lattice-host`) holds the per-app `CompletionRegistry`
/// and stores registrations there. Both surfaces — picker and
/// cmdline-completion — consume registrations through the
/// registry.
///
/// **Two lifecycles** (cross-check § "Two registration
/// lifecycles"):
///
/// - **Persistent**: registered at boot via
///   `CompletionRegistry::register_source`, lives until
///   shutdown. First-party Files / Buffers / Commands etc.
///   Plugin sources whose data is synchronously enumerable.
///
/// - **Transient**: constructed per-use by the host (LSP
///   pickers building from async responses) or a plugin (async-
///   fetch), passed directly to `Picker::open_with(reg)`,
///   dropped after accept/dismiss. Same shape; just shorter
///   lifetime.
pub struct SourceRegistration {
    pub spec: SourceSpec,
    pub kind: CandidateSourceKind,
    /// Accept handler. `None` ⇒ candidate selection is
    /// effectively a no-op (rare; cmdline-completion sources
    /// today don't carry an explicit handler — the accept logic
    /// is inline. Slice 7c migrates those to explicit handlers
    /// returning `AcceptAction::InsertText`).
    pub accept: Option<Arc<dyn AcceptHandler>>,
    /// Per-source matcher override. `None` ⇒ use the registry
    /// default.
    pub matcher_override: Option<MatcherId>,
    /// Per-source ranker chain override. Empty ⇒ use the
    /// registry default chain.
    pub ranker_overrides: Vec<RankerId>,
    /// Additional annotators to run on this source's
    /// candidates, in registration order. Appended to (not
    /// replacing) the registry default annotators.
    pub annotator_extras: Vec<AnnotatorId>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::candidate::{CandidateKind, RawCandidate};

    /// Construct a registration with the minimum fields; verifies
    /// the shape compiles + the variants are reachable.
    #[test]
    fn presupplied_registration_compiles() {
        let rows = vec![RawCandidate::plain("hi", CandidateKind::Plain)];
        let reg = SourceRegistration {
            spec: SourceSpec {
                id: "test:probe".to_string(),
                doc: "probe".to_string(),
                args_schema: None,
                live: false,
            },
            kind: CandidateSourceKind::PreSupplied(Arc::new(rows)),
            accept: None,
            matcher_override: None,
            ranker_overrides: Vec::new(),
            annotator_extras: Vec::new(),
        };
        assert_eq!(reg.spec.id, "test:probe");
        assert!(matches!(reg.kind, CandidateSourceKind::PreSupplied(_)));
    }

    /// `AcceptAction::Custom` constructs and survives a Debug
    /// print without panicking.
    #[test]
    fn accept_action_custom_payload_debug_prints() {
        let action = AcceptAction::Custom(CustomAcceptPayload(Box::new(42_i32)));
        let s = format!("{action:?}");
        assert!(s.contains("Custom"));
        assert!(s.contains("opaque"));
    }

    /// `AcceptToken` is Ord + Hash so the host can use it as a
    /// HashMap key (e.g. pending-table lookup).
    #[test]
    fn accept_token_is_hash_and_ord() {
        let a = AcceptToken::new(1);
        let b = AcceptToken::new(2);
        assert!(a < b);
        let mut set = std::collections::HashSet::new();
        set.insert(a);
        set.insert(b);
        assert_eq!(set.len(), 2);
    }

    /// Stateless variants round-trip through Debug.
    #[test]
    fn accept_action_stateless_variants_debug() {
        let actions = [
            AcceptAction::OpenFile {
                path: PathBuf::from("/tmp/x"),
            },
            AcceptAction::SwitchBuffer {
                id: lattice_core::BufferId(7),
            },
            AcceptAction::PasteRegister { name: 'a' },
            AcceptAction::JumpToMark { name: 'm' },
        ];
        for a in actions {
            let _ = format!("{a:?}");
        }
    }
}
