//! M.4 follow-up: help-topic registry moved to
//! `lattice-help::topics`. This shim re-exports the public surface
//! so the existing `crate::help_topics::*` callsites keep
//! compiling. New callers should import from
//! `lattice_help::topics` directly.

pub use lattice_help::topics::*;
