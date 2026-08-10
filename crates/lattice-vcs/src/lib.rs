//! Pure git data layer — read + write operations over a git repository.
//!
//! Wraps [`gix`] for repository interaction, providing a typed API surface
//! consumed by the VCS subsystem (`lattice-host::vcs`) and the magit feature
//! buffer modes (`lattice-magit`).
//!
//! Zero `lattice-*` dependencies. Only `gix`, `ropey`, `smallvec`, `thiserror`.
//!
//! # Example
//!
//! ```no_run
//! use lattice_vcs::{Repository, WorkingTree};
//!
//! let repo = Repository::discover(".").unwrap();
//! let statuses = WorkingTree::statuses(&repo).unwrap();
//! for (path, status) in statuses {
//!     println!("{}: {:?}", path.display(), status);
//! }
//! ```

#![deny(unsafe_code)]

mod bisect;
mod blob;
mod branch;
mod commit;
mod error;
mod index;
mod note;
mod reference;
mod remote;
mod repository;
mod stash;
mod submodule;
mod working_tree;

pub use bisect::{Bisect, BisectState, parse_bisect_vars};
pub use blob::GitBlob;
pub use branch::Branch;
pub use commit::Commit;
pub use error::{Result, VcsError};
pub use index::Index;
pub use note::{Note, NoteMergeStrategy};
pub use reference::{RefEntry, RefKind, Reference, parse_for_each_ref};
pub use remote::{Remote, RemoteEntry, parse_remote_v};
pub use repository::Repository;
pub use stash::Stash;
pub use submodule::{Submodule, SubmoduleEntry, SubmoduleState, parse_submodule_status};
pub use working_tree::{PathChange, PathStatus, UnmergedKind, WorkingTree};
