//! Editor-thread handle onto the AI supervisor task (AI-1b).
//!
//! `AiClientHandle` is the only thing the editor thread touches: it sends
//! `AiCmd`s into the supervisor's channel and reads an `ArcSwap<AiState>`
//! snapshot. Both are non-blocking -- no protocol I/O, no locks held across
//! `.await`, ever runs on the editor thread. The supervisor itself (owning
//! the provider child, `Connection`, and `SessionId`) lives in
//! `supervisor.rs`.

use std::sync::Arc;

use arc_swap::ArcSwap;
use tokio::sync::mpsc;

use lattice_agent::SessionKey;

use crate::acp::providers::ProviderConfig;

/// Editor-visible snapshot of the active AI session, if any. Cheap to clone
/// and compare; read via [`AiClientHandle::snapshot`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AiState {
    pub running: bool,
    pub provider: Option<&'static str>,
    /// Active session key (provider + per-provider index), if a session is
    /// open.
    pub session: Option<SessionKey>,
}

/// Commands the handle sends into the supervisor's command loop.
pub(crate) enum AiCmd {
    Start(ProviderConfig),
    Prompt(String),
    /// AU‑3: interrupt the active turn without ending the session. The
    /// supervisor forwards an ACP `session/cancel`; the session stays open.
    Interrupt,
    Stop,
}

/// Clone-able handle onto a running (or idle) AI supervisor task.
///
/// Fields are `pub(crate)` so a later task's `commands.rs` tests can build a
/// handle directly from a channel + `ArcSwap` without going through
/// `spawn`.
#[derive(Clone)]
pub struct AiClientHandle {
    pub(crate) cmd_tx: mpsc::UnboundedSender<AiCmd>,
    pub(crate) state: Arc<ArcSwap<AiState>>,
}

impl AiClientHandle {
    /// Ask the supervisor to start `provider`. Non-blocking; the result
    /// surfaces later via [`AiClientHandle::snapshot`] and the provider's
    /// `AiLogger` ring.
    pub fn start(&self, provider: ProviderConfig) {
        let _ = self.cmd_tx.send(AiCmd::Start(provider));
    }

    /// Ask the supervisor to send `text` as a prompt on the active session.
    /// Non-blocking; if no session is open the supervisor drops the prompt
    /// and logs a `Warn`-level "prompt dropped: no active session" record
    /// to the subsystem-wide `AiLogger` ring instead of sending it.
    pub fn prompt(&self, text: String) {
        let _ = self.cmd_tx.send(AiCmd::Prompt(text));
    }

    /// AU‑3: interrupt the active turn without ending the session.
    /// Non-blocking; the supervisor forwards an ACP `session/cancel`. If no
    /// session is open it's a no-op. Distinct from [`AiClientHandle::stop`],
    /// which tears the session (and provider child) down.
    pub fn interrupt(&self) {
        let _ = self.cmd_tx.send(AiCmd::Interrupt);
    }

    /// Ask the supervisor to stop the active session. Non-blocking.
    pub fn stop(&self) {
        let _ = self.cmd_tx.send(AiCmd::Stop);
    }

    /// Read the current state snapshot. Never blocks.
    pub fn snapshot(&self) -> AiState {
        (**self.state.load()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_snapshot_and_nonblocking_sends() {
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let state = Arc::new(ArcSwap::from_pointee(AiState::default()));
        let handle = AiClientHandle { cmd_tx, state };

        assert_eq!(handle.snapshot(), AiState::default());

        // Drop the receiver -- sends must not panic even though nothing is
        // listening.
        drop(cmd_rx);
        handle.prompt("hi".into());
        handle.stop();
    }
}
