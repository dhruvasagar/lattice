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
    /// Set via `:set`, `init.toml`, or user TOML (global config layer).
    GlobalConfig,
    /// Set via `:setlocal` for this buffer.
    BufferLocal,
    /// Contributed by a mode (major or minor).
    ModeContribution { mode_id: String },
}

impl std::fmt::Display for OptionOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => f.write_str("default"),
            Self::GlobalConfig => f.write_str("global"),
            Self::BufferLocal => f.write_str("buffer-local"),
            Self::ModeContribution { mode_id } => write!(f, "mode: {mode_id}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_variants() {
        assert_eq!(OptionOrigin::Default.to_string(), "default");
        assert_eq!(OptionOrigin::GlobalConfig.to_string(), "global");
        assert_eq!(OptionOrigin::BufferLocal.to_string(), "buffer-local");
        assert_eq!(
            OptionOrigin::ModeContribution {
                mode_id: "rust-mode".into()
            }
            .to_string(),
            "mode: rust-mode"
        );
    }
}
