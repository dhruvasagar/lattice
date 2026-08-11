//! Pure data layer for Lattice's diff subsystem.
//!
//! Arity-agnostic hunk computation over `&ropey::Rope` inputs,
//! backed by [`imara_diff`] (Histogram by default; Myers +
//! Patience also wired). No host integration, no actor
//! plumbing -- those land in D.2.
//!
//! [`compute_diff`] is the only public engine entry: pass an
//! `&[Rope]` of N participants (N ∈ {0, 1, 2, 3} in v1), get
//! back a `Result<HunkIndex, DiffEngineError>`. The engine
//! dispatches by arity to crate-private two-way / three-way
//! algorithms; external consumers never branch on N. See
//! `docs/dev/architecture/n-way-diff-membership.md` (D.8) for
//! the design rationale.
//!
//! See `docs/dev/architecture/diff-system.md` §3.1 for the type
//! model and §4 for the engine choice.
//!
//! ## Example
//!
//! ```
//! use lattice_diff::{compute_diff, DiffAlgorithm, HunkKind};
//! use ropey::Rope;
//!
//! let a = Rope::from("alpha\nbeta\ngamma\n");
//! let b = Rope::from("alpha\nBETA\ngamma\n");
//! let hunks = compute_diff(&[a, b], DiffAlgorithm::Histogram)
//!     .expect("two-way is supported");
//!
//! assert_eq!(hunks.hunks.len(), 1);
//! assert_eq!(hunks.hunks[0].kind, HunkKind::Change);
//! ```

#![deny(unsafe_code)]

pub mod compute;
pub mod patch;
// DR.1 (2026-08-12): intra-line refinement — which PART of a changed
// line changed. Pure; `lattice-magit` and `diff-mode` both consume it.
pub mod refine;
pub mod types;

// BC.6 / DX.6 (2026-06-24): the host-attached diff *subsystem*, moved
// in from `lattice-host::diff`. The pure data layer above
// (`compute`/`types`/`patch`) stays the bottom of the crate; these
// layer the sessions + `diff-mode` + presentation providers on top.
// `lattice-host` re-exports these modules under `crate::diff::*` (the
// façade) so existing call sites are unchanged; DX.7 collapses the boot
// wiring into `lattice_diff::install(boot)`.
pub mod filler;
pub mod fold;
pub mod mode;
// D-fix.5: diff-presentation options (`ui.diff.fold-unchanged`,
// `ui.diff.context`). Self-register via `linkme` at link time; the
// module exists so the crate links the registrations into the binary.
pub mod options;
pub mod overlay;
pub mod pane_group;
pub mod subsystem;

// BC.6 / DX.7: the crate-owned `install(boot)` entry point — one Phase-B
// line in `editor_boot` (`lattice_diff::install(&mut boot)`), the terminal /
// claude-code shape. Registers the diff modes; the subsystem bind + keymap
// push + modeline element stay host-side (see `install` docs for why).
pub mod install;

// I4 (Claude Code IDE peer, openDiff): the host-drained "open a diff and await
// the user's verdict" request + its inbound bus. The host drains it and opens a
// side-by-side diff bound to the request's completion oneshot; the IDE peer (or
// any in-tree consumer: an LSP WorkspaceEdit preview, a magit-style plugin)
// produces it.
pub mod programmatic;

pub use compute::{DiffEngineError, compute_diff};
pub use install::install;
pub use programmatic::{ProgrammaticDiffBus, ProgrammaticDiffRequest};
pub use refine::{LineRefinement, refine_pair, refine_runs};
pub use types::{DiffAlgorithm, Hunk, HunkIndex, HunkKind, LineRange};
