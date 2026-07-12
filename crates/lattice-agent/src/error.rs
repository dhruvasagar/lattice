//! Error surface for the agent capability port.

/// Failures raised by [`crate`]'s port operations. Distinct from an adapter's
/// protocol error: this describes the *editor* refusing or failing, never a
/// wire fault.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// Nothing is draining the bus on the host side — a boot misconfiguration,
    /// or the editor is shutting down.
    #[error("editor not reachable: {0}")]
    Bus(String),
    /// A review's reply channel dropped before the user decided (the diff was
    /// closed, the session died, the editor went away).
    #[error("cancelled: {0}")]
    Cancelled(String),
    /// A read or write against the editor failed.
    #[error("editor io error: {0}")]
    Io(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, AgentError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_is_prefixed() {
        assert_eq!(
            AgentError::Bus("no receiver".into()).to_string(),
            "editor not reachable: no receiver"
        );
        assert_eq!(
            AgentError::Cancelled("diff closed".into()).to_string(),
            "cancelled: diff closed"
        );
        assert_eq!(
            AgentError::Io("read failed".into()).to_string(),
            "editor io error: read failed"
        );
    }
}
