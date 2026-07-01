//! Lattice launch **dashboard** — the branded start page shown when the editor
//! opens with no file argument.
//!
//! This crate (DB.1) is the pure library core: the [`DashboardSection`] trait,
//! the [`DashboardRegistry`] extensibility seam, the [`DashboardFragment`]
//! content contract, the eight built-in sections, and the `dashboard.*` config
//! options. The editor wiring — the read-only `*dashboard*` buffer, the
//! `dashboard-mode` major mode, the `:dashboard` command, the `dashboard.*`
//! theme elements, the branding terminal-art, and the startup trigger — lands
//! in later slices (DB.2+).
//!
//! Design: `docs/dev/architecture/dashboard.md`. Slice plan:
//! `docs/dev/operations/slice-plans/dashboard.md`.

pub mod fragment;
pub mod install;
pub mod mode;
pub mod options;
pub mod registry;
pub mod section;
pub mod sections;

pub use fragment::{Align, DashboardFragment, DashboardRole, DashboardRow, DashboardSpan, LinkTarget};
pub use install::install;
pub use mode::{register_dashboard_modes, DashboardMode};
pub use options::{Dashboard, DashboardEnabled, DashboardSections, DashboardSource};
pub use registry::{DashboardRegistry, SectionSelection};
pub use section::{DashboardCtx, DashboardSection};
pub use sections::builtin_registry;
