//! Provider launch configs — the crate's extension point. Adding an agent is a
//! new constructor here, not a new subsystem.

/// How to launch one ACP agent as a stdio subprocess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderConfig {
    /// Executable to spawn.
    pub command: String,
    /// Arguments that put the agent into ACP-over-stdio mode.
    pub args: Vec<String>,
    /// Extra environment injected into the child.
    pub env: Vec<(String, String)>,
    /// Human-readable name (modeline / echoes).
    pub display_name: &'static str,
}

impl ProviderConfig {
    /// opencode's native ACP entry (exact args confirmed in Task 0's spike).
    pub fn opencode() -> Self {
        Self {
            command: "opencode".to_string(),
            args: vec!["acp".to_string()],
            env: Vec::new(),
            display_name: "opencode",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_config_names_the_binary_and_display() {
        let p = ProviderConfig::opencode();
        assert_eq!(p.command, "opencode");
        assert_eq!(p.display_name, "opencode");
        assert!(!p.args.is_empty(), "must pass the ACP-mode flag");
    }
}
