//! Filesystem entry listings as buffers — the two views the editor
//! offers over a directory tree.
//!
//! - [`oil`] — a flat, **editable** listing of one directory
//!   (oil.nvim-style). `:w` diffs the rope against a snapshot and
//!   executes the renames / deletes / creates it implies.
//! - [`file_tree`] — a hierarchical, **read-only** tree with
//!   expand/collapse.
//!
//! ## Why one crate (DL.0)
//!
//! These shipped as `lattice-oil` and `lattice-file-tree` and were
//! merged, because they are the same domain seen twice rather than two
//! features that happen to be adjacent:
//!
//! - no consumer ever took one without the other — `lattice-host` and
//!   both renderers depend on both, so the split bought no modularity
//!   anyone used;
//! - both carried a directory reader of near-identical shape (DL.7
//!   converged them onto [`dir::read_dir_sorted`]);
//! - their entry models (`oil::OilEntry`, `file_tree::FileTreeEntry`)
//!   are one concept in two shapes.
//!
//! That duplication is what let a single arithmetic slip live in four
//! paint paths (CV.5) and is what the `directory-listing-mode` work
//! (see `docs/dev/architecture/directory-listing-mode.md`) exists to
//! remove: one minor mode, activated on both majors, owning the icons
//! and the theme-rooted highlighting they both need.
//!
//! ## What deliberately did NOT converge (DL.7)
//!
//! The merge's stated debt was "two directory readers, two rope
//! renderers, two entry models". Only the first was real:
//!
//! - **Entry models stay separate.** [`file_tree::FileTreeEntry`]
//!   carries `depth` and expansion state because a tree has both;
//!   [`oil::OilEntry`] is a flat, editable row and has neither.
//!   Merging them would either bloat oil with fields that mean nothing
//!   there, or drop the state the tree needs — and the shared shape
//!   they *both* project into already exists
//!   ([`listing_mode::ListingEntry`]), which is what the presentation
//!   layer consumes. There is nothing left to unify.
//! - **Rope renderers stay separate.** Oil's is bare names joined by
//!   newlines, because `:w` diffs that text back into filesystem
//!   operations; the tree's is indent + expand marker + name. Two
//!   formats, deliberately. What actually mattered — that neither
//!   embeds icons — is already true, and is what let both move to the
//!   shared paint path.
//!
//! Recorded because "three duplications" reads like unfinished work
//! otherwise. Two of the three were never duplication.
//!
//! **The merge itself changed no behaviour.** The duplicated readers,
//! renderers and entry models are still duplicated here, deliberately —
//! converging them is DL.6, and folding it into the move would have
//! made the move unreviewable. See the slice plan.

pub mod dir;
pub mod file_tree;
pub mod listing_mode;
pub mod oil;
