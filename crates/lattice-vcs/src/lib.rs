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

mod blob;
mod branch;
mod commit;
mod error;
mod index;
mod reference;
mod remote;
mod repository;
mod stash;
mod working_tree;

pub use blob::GitBlob;
pub use branch::Branch;
pub use commit::Commit;
pub use error::{Result, VcsError};
pub use index::Index;
pub use reference::Reference;
pub use remote::{Remote, RemoteEntry, parse_remote_v};
pub use repository::Repository;
pub use stash::Stash;
pub use working_tree::{PathStatus, WorkingTree};
