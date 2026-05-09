//! `App` struct + Action enum + tightly-coupled supporting
//! types. The single source of "what state does the App
//! carry" -- a reader who wants the data layout reads only
//! this file.
//!
//! Owned types that move here in R.1:
//! - `App` struct definition (currently in `app.rs`).
//! - `Action` enum.
//! - `OptionCache`.
//! - `Fold`, `FindKind`, `EchoLevel`, `EchoMessage`,
//!   `SearchLine`, `LastSearch`, `LastFind`, `LastVisual`,
//!   `MacroRecording`, `TagStackEntry`, `PositionEntry`,
//!   `PositionSource`, `ReplaceEntry`, `SubstitutePreview`,
//!   `PendingBlockInsert`, `UnnamedRegister`, `PrevPaneState`.
//! - LSP-result outcome enums that define App's response
//!   shape (`HoverOutcome`, `ReferencesOutcome`, etc.) --
//!   these are state-shape types, not feature logic, so they
//!   live with state. The handlers consuming them live in
//!   `lsp.rs`.
//!
//! Methods do NOT live here. State definitions only.
//! Per-feature `impl App { ... }` blocks live in the
//! corresponding `app/<feature>.rs`.
