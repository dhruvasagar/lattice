//! Quit guards: work a subsystem would lose if the editor quit now.
//!
//! `:q` on the last window and `:qa` refuse while a buffer has unsaved
//! changes; that guard is the host's, because buffers are. Work that is not a
//! buffer has an owner the host knows nothing about: an LLM request streaming
//! an answer, a job in flight. That owner registers a check here, and the host
//! consults every check after its own, before quitting. A check returns why
//! quitting would lose something, or `None`; the first reason refuses the
//! quit and is shown with "(add ! to override)". `!` skips every guard, as it
//! skips the dirty one.
//!
//! A check runs on the editor thread inside `:q`, so it must be cheap and
//! never block: read a snapshot, count, answer.

use std::sync::{Arc, RwLock};

/// One check: why quitting now would lose work, if it would.
pub type QuitCheck = Arc<dyn Fn() -> Option<String> + Send + Sync>;

/// The registered checks, in registration order.
#[derive(Default)]
pub struct QuitGuards {
    checks: RwLock<Vec<(&'static str, QuitCheck)>>,
}

/// The `ServiceRegistry` registration and lookup type.
pub type QuitGuardsHandle = Arc<QuitGuards>;

impl QuitGuards {
    /// No checks: quitting is refused by nothing here.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `check` under `owner` (a subsystem's name, for logs). A second
    /// registration under the same owner replaces the first.
    pub fn register(&self, owner: &'static str, check: QuitCheck) {
        let mut checks = self.checks.write().unwrap_or_else(|e| e.into_inner());
        checks.retain(|(o, _)| *o != owner);
        checks.push((owner, check));
    }

    /// The first reason a check gives to refuse quitting, if any.
    pub fn refusal(&self) -> Option<String> {
        let checks = self.checks.read().unwrap_or_else(|e| e.into_inner());
        checks.iter().find_map(|(_, check)| check())
    }
}

impl std::fmt::Debug for QuitGuards {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let checks = self.checks.read().unwrap_or_else(|e| e.into_inner());
        f.debug_struct("QuitGuards")
            .field(
                "owners",
                &checks.iter().map(|(o, _)| *o).collect::<Vec<_>>(),
            )
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_reason_refuses_and_none_allows() {
        let guards = QuitGuards::new();
        assert_eq!(guards.refusal(), None);
        guards.register("idle", Arc::new(|| None));
        assert_eq!(guards.refusal(), None);
        guards.register("busy", Arc::new(|| Some("2 jobs running".to_string())));
        assert_eq!(guards.refusal().as_deref(), Some("2 jobs running"));
    }

    #[test]
    fn registering_again_replaces_the_owners_check() {
        let guards = QuitGuards::new();
        guards.register("x", Arc::new(|| Some("old".to_string())));
        guards.register("x", Arc::new(|| None));
        assert_eq!(guards.refusal(), None);
    }
}
