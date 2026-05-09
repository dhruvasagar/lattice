//! Oil-buffer App surface -- opening, navigation, the
//! `:write` apply path that diffs the rope vs the snapshot
//! and runs filesystem ops.
//!
//! Methods that move here in R.1:
//! - `do_open_oil` (the `:Oil` / `<C-w>e` entry).
//! - `do_oil_follow` (`<CR>` on a row).
//! - The oil-arm of `do_write` (the diff-and-apply path).
//! - `seed_oil_locals` (M.3.2.c.3 mirror at creation).
//!
//! What does NOT live here: `OilBuffer` itself
//! (`crate::oil::OilBuffer`), the diff algorithm, the
//! filesystem-op planner -- those are content-shape
//! concerns owned by `crate::oil`.
