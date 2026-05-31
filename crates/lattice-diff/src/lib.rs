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
pub mod types;

pub use compute::{compute_diff, DiffEngineError};
pub use types::{DiffAlgorithm, Hunk, HunkIndex, HunkKind, LineRange};
