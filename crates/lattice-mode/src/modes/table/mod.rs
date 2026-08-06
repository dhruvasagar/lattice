//! Markdown / org table support.
//!
//! Today this is [`layout`] alone — parse a pipe table, measure each
//! cell by the columns it will actually occupy, and pad so the columns
//! line up. `lattice-help` calls it when it builds a help buffer.
//!
//! It lives here rather than in `lattice-help` because the next
//! consumer is a `table-mode` in this directory: an org-table-style
//! minor mode that realigns as you type, moves between cells, and
//! inserts and deletes rows and columns. That mode needs exactly this
//! parse-measure-pad core, and a mode reaching into the help crate for
//! it — or, worse, growing a second copy — is the duplication failure
//! this placement avoids. The two consumers differ only in *when* they
//! run it: help once at content-build, table-mode on every edit.

pub mod layout;
