//! [`OptionOrigin`] — tracks which resolution layer supplied the
//! effective value for a resolved option (`buffer-local-options.md` §4.4).
//!
//! Carried alongside each entry in [`crate::ResolvedOptions`] so
//! `:set name?` / `:setlocal name?` can echo the origin in parens.

/// The layer that supplied the winning value in a resolution cycle.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OptionOrigin {
    /// The option's registered default — no override at any layer.
    #[default]
    Default,
    /// Set via `:set` or `lattice.toml` (global config layer).
    GlobalConfig,
    /// Set via `:setlocal` for this buffer.
    BufferLocal,
    /// Contributed by a mode (major or minor).
    ModeContribution {
        /// The contributing mode's id (`rust-mode`), echoed as
        /// `(mode: rust-mode)`.
        mode_id: String,
    },
    /// Supplied by a named profile (LL.7): a bundle of starting values a
    /// subsystem selects, echoed as `(profile: writer)`.
    Profile {
        /// The profile's name.
        name: String,
    },
    /// Supplied by a profile the subsystem ships (AT.4), under any user
    /// profile of the same name, echoed as `(builtin profile: coding)`.
    BuiltinProfile {
        /// The profile's name.
        name: String,
    },
    /// Supplied by project-level config checked in beside the code (LL.7),
    /// which is untrusted and so subject to a trust table.
    Project,
}

impl std::fmt::Display for OptionOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::GlobalConfig => f.write_str("global"),
            Self::BufferLocal => f.write_str("buffer-local"),
            Self::ModeContribution { mode_id } => write!(f, "mode: {mode_id}"),
            Self::Profile { name } => write!(f, "profile: {name}"),
            Self::BuiltinProfile { name } => write!(f, "builtin profile: {name}"),
            Self::Project => f.write_str("project"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_variants() {
        assert_eq!(OptionOrigin::Default.to_string(), "default");
        assert_eq!(
            OptionOrigin::BuiltinProfile {
                name: "coding".into()
            }
            .to_string(),
            "builtin profile: coding"
        );
        assert_eq!(OptionOrigin::GlobalConfig.to_string(), "global");
        assert_eq!(OptionOrigin::BufferLocal.to_string(), "buffer-local");
        assert_eq!(
            OptionOrigin::ModeContribution {
                mode_id: "rust-mode".into()
            }
            .to_string(),
            "mode: rust-mode"
        );
        assert_eq!(
            OptionOrigin::Profile {
                name: "writer".into()
            }
            .to_string(),
            "profile: writer"
        );
        assert_eq!(OptionOrigin::Project.to_string(), "project");
    }
}
