//! Editor-thread handle onto the AI supervisor task (AI-1b).
//!
//! `AiClientHandle` is the only thing the editor thread touches: it sends
//! `AiCmd`s into the supervisor's channel and reads an `ArcSwap<AiState>`
//! snapshot. Both are non-blocking -- no protocol I/O, no locks held across
//! `.await`, ever runs on the editor thread. The supervisor itself (owning
//! the provider child, `Connection`, and `SessionId`) lives in
//! `supervisor.rs`.

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;

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
    /// AU‑5: trust mode. `false` (the default) is **review** mode — file edits
    /// are gated on a diff verdict and un-reviewable mutating ops are denied.
    /// `true` auto-grants every permission request without the diff gate.
    pub auto_accept: bool,
    /// AUX‑4: number of prompts currently queued behind an in-flight one.
    pub queue_len: usize,
}

/// Commands the handle sends into the supervisor's command loop.
pub(crate) enum AiCmd {
    Start(ProviderConfig),
    Prompt(String),
    /// AU‑3: interrupt the active turn without ending the session. The
    /// supervisor forwards an ACP `session/cancel`; the session stays open.
    Interrupt,
    /// AU‑5: set trust mode. `true` auto-grants every permission request; `false`
    /// restores review mode (diff-gated edits, denied un-reviewable ops).
    SetAutoAccept(bool),
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
    /// AUX‑4: shared with the supervisor loop; read by the headerline renderer.
    pub queue_len: Arc<AtomicUsize>,
}

impl AiClientHandle {
    /// AUX‑4: expose the live queue length for the headerline.
    pub fn queue_len(&self) -> usize {
        self.queue_len.load(std::sync::atomic::Ordering::Relaxed)
    }

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

    /// AU‑5: set trust mode. Non-blocking; the supervisor applies the flag and
    /// republishes `AiState`.
    pub fn set_auto_accept(&self, on: bool) {
        let _ = self.cmd_tx.send(AiCmd::SetAutoAccept(on));
    }

    /// AU‑5: flip trust mode, returning the value it was flipped to (from the
    /// current snapshot). Non-blocking; the supervisor applies + republishes.
    pub fn toggle_auto_accept(&self) -> bool {
        let next = !self.snapshot().auto_accept;
        self.set_auto_accept(next);
        next
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
        let handle = AiClientHandle {
            cmd_tx,
            state,
            queue_len: Arc::new(AtomicUsize::new(0)),
        };

        assert_eq!(handle.snapshot(), AiState::default());

        // Drop the receiver -- sends must not panic even though nothing is
        // listening.
        drop(cmd_rx);
        handle.prompt("hi".into());
        handle.stop();
    }

    /// AU‑5: `toggle_auto_accept` flips against the current snapshot, returns
    /// the new value, and sends a matching `SetAutoAccept`.
    #[test]
    fn toggle_auto_accept_flips_and_sends() {
        let (cmd_tx, mut cmd_rx) = mpsc::unbounded_channel();
        let state = Arc::new(ArcSwap::from_pointee(AiState::default()));
        let handle = AiClientHandle {
            cmd_tx,
            state,
            queue_len: Arc::new(AtomicUsize::new(0)),
        };

        // Default is review mode (false) → toggling turns trust on.
        assert!(handle.toggle_auto_accept());
        assert!(matches!(cmd_rx.try_recv(), Ok(AiCmd::SetAutoAccept(true))));

        // Reflect the applied state, then toggle back off.
        handle.state.store(Arc::new(AiState { auto_accept: true, ..AiState::default() }));
        assert!(!handle.toggle_auto_accept());
        assert!(matches!(cmd_rx.try_recv(), Ok(AiCmd::SetAutoAccept(false))));
    }

    // ── AUX‑4: queue_len ──

    #[test]
    fn queue_len_defaults_zero() {
        let handle = AiClientHandle {
            cmd_tx: mpsc::unbounded_channel().0,
            state: Arc::new(ArcSwap::from_pointee(AiState::default())),
            queue_len: Arc::new(AtomicUsize::new(0)),
        };
        assert_eq!(handle.queue_len(), 0);
        assert_eq!(handle.snapshot().queue_len, 0);
    }

    #[test]
    fn queue_len_accessible_on_handle() {
        let ql = Arc::new(AtomicUsize::new(3));
        let handle = AiClientHandle {
            cmd_tx: mpsc::unbounded_channel().0,
            state: Arc::new(ArcSwap::from_pointee(AiState::default())),
            queue_len: ql.clone(),
        };
        assert_eq!(handle.queue_len(), 3);
        // Mutating the atomic is reflected in the handle's live reader.
        ql.store(5, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(handle.queue_len(), 5);
    }
}
