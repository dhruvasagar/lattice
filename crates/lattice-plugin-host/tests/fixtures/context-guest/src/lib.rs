//! TC.2 context fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `context-plugin` world,
//! driving the context producer actor (`context_task.rs`) through a real
//! host→guest `context-scopes` call. It exercises the three contracts the seam
//! promises:
//!
//!   - **The tree crosses as a usable borrow.** Scopes are derived by actually
//!     walking the handed `tree-snapshot` — one scope per named child of the
//!     root, spanning that child's line range. A fixture that returned
//!     constants would pass even if the borrow arrived dead, which is exactly
//!     the thing worth proving here: this is the repo's first `borrow<>` across
//!     an ASYNC export.
//!   - **No tree is a normal state, not an error.** `none` (plain text, or a
//!     parse still pending) returns an EMPTY list, not an `err` — so the host
//!     caches "no scopes" rather than keeping stale ones.
//!   - **Graceful failure.** An empty buffer (`line_count == 0`) returns the WIT
//!     typed `err`, exercising the path where the host keeps the buffer's prior
//!     cached scopes rather than blanking the strip (§8).

wit_bindgen::generate!({
    world: "context-plugin",
    path: "../../../../../wit",
});

use exports::lattice::plugin_host::context::Guest;
use lattice::plugin_host::tree_sitter::TreeSnapshot;
use lattice::plugin_host::types::{ContextRequest, ContextScope};

struct Component;

impl Guest for Component {
    fn context_scopes(
        req: ContextRequest,
        tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<ContextScope>, String> {
        if req.line_count == 0 {
            // Graceful: nothing to scan → a typed guest err, not a trap.
            return Err("empty buffer: no context".to_string());
        }
        // No parse is not a failure — the host should cache an empty set.
        let Some(tree) = tree else {
            return Ok(Vec::new());
        };

        // Walk the tree for real. Each named child of the root becomes a scope
        // spanning its own lines, with its first line as the header.
        let root = tree.root();
        let count = root.named_child_count();
        let mut scopes = Vec::new();
        for i in 0..count {
            let Some(child) = root.named_child(i) else {
                continue;
            };
            let r = child.byte_range();
            scopes.push(ContextScope {
                scope_start: r.start.line,
                scope_end: r.end.line,
                header_start: r.start.line,
                header_end: r.start.line,
            });
        }
        Ok(scopes)
    }
}

export!(Component);
