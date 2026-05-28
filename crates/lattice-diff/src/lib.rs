//! Pure data layer for Lattice's diff subsystem.
//!
//! Two-way and three-way hunk computation over `&ropey::Rope`
//! inputs, backed by [`imara_diff`] (Histogram by default; Myers
//! + Patience also wired). No host integration, no actor
//! plumbing -- those land in D.2.
//!
//! See `docs/dev/architecture/diff-system.md` §3.1 for the type
//! model and §4 for the engine choice.
//!
//! ## Example
//!
//! ```
//! use lattice_diff::{compute_two_way, DiffAlgorithm, HunkKind};
//! use ropey::Rope;
//!
//! let a = Rope::from("alpha\nbeta\ngamma\n");
//! let b = Rope::from("alpha\nBETA\ngamma\n");
//! let hunks = compute_two_way(&a, &b, DiffAlgorithm::Histogram);
//!
//! assert_eq!(hunks.hunks.len(), 1);
//! assert_eq!(hunks.hunks[0].kind, HunkKind::Change);
//! ```

#![deny(unsafe_code)]

pub mod compute;
pub mod patch;
pub mod types;

pub use compute::{compute_three_way, compute_two_way};
pub use types::{DiffAlgorithm, Hunk, HunkIndex, HunkKind, LineRange};
