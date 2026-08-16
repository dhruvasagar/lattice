//! The sticky-context boundary conversions (treesitter-context.md, TC.2).
//!
//! Mirrors `lattice_cells::context::ContextScope` — the structural scopes a
//! context provider produces. Two directions:
//!
//!   - **`ContextScope`** crosses **guest→host** (the producer's return): a
//!     [`WitBoundary`] round-trip. Four `u32`s per scope and nothing else, which
//!     is the point — a whole file's structure is a few tens of kB, and the tree
//!     itself never crosses.
//!   - **`context-request`** crosses **host→guest** (one-way, the
//!     `project_decoration_context` precedent). The host has the buffer metadata
//!     off the render path when it triggers the producer; the parse tree rides a
//!     call-scoped `borrow<tree-snapshot>` alongside, not this record.

use crate::WitBoundary;
use crate::lattice::plugin_host::types::{
    ContextRequest as WitContextRequest, ContextScope as WitContextScope,
};
use lattice_cells::context::ContextScope as NativeContextScope;

impl WitBoundary for NativeContextScope {
    type Wit = WitContextScope;

    fn to_wit(&self) -> Result<WitContextScope, String> {
        Ok(WitContextScope {
            scope_start: self.scope_start,
            scope_end: self.scope_end,
            header_start: self.header_start,
            header_end: self.header_end,
        })
    }

    fn from_wit(wit: WitContextScope) -> Result<Self, String> {
        Ok(NativeContextScope {
            scope_start: wit.scope_start,
            scope_end: wit.scope_end,
            header_start: wit.header_start,
            header_end: wit.header_end,
        })
    }
}

/// Build the owned `context-request` the host hands a producer (host→guest,
/// one-way).
///
/// A non-UTF-8 path drops to `None` rather than failing the trigger: a context
/// producer keys off the *tree*, not the path text, so an un-representable path
/// costs it nothing. (Same call as `project_decoration_context` makes, for the
/// same reason.)
pub fn project_context_request(
    buffer_id: u64,
    path: Option<&std::path::Path>,
    line_count: u32,
) -> WitContextRequest {
    WitContextRequest {
        buffer_id,
        path: path.and_then(|p| p.to_str().map(str::to_string)),
        line_count,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn context_scope_round_trips_all_four_lines() {
        // Distinct values in every field: a transposition (header_start and
        // scope_start swapped, say) is the plausible mistake here, and equal
        // values would hide it.
        let native = NativeContextScope {
            scope_start: 10,
            scope_end: 99,
            header_start: 11,
            header_end: 13,
        };

        let back = NativeContextScope::from_wit(native.to_wit().unwrap()).unwrap();

        assert_eq!(back, native);
    }

    #[test]
    fn context_request_projects_metadata() {
        let req = project_context_request(9, Some(std::path::Path::new("src/lib.rs")), 240);
        assert_eq!(req.buffer_id, 9);
        assert_eq!(req.path.as_deref(), Some("src/lib.rs"));
        assert_eq!(req.line_count, 240);

        // A pathless (scratch) buffer projects `None` rather than failing.
        let scratch = project_context_request(1, None, 0);
        assert!(scratch.path.is_none());
    }
}
