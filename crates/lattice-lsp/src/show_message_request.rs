//! Server-initiated `window/showMessageRequest` plumbing
//! (4.4.b; BC.8e reshape).
//!
//! Spec (LSP §3.16): the server emits a message at a severity
//! level (`Error` / `Warning` / `Info` / `Log`) accompanied by a
//! list of `MessageActionItem` action labels. The client
//! displays a modal picker; the user selects one (or dismisses);
//! the client replies with the selected `MessageActionItem`, or
//! `null` if the user dismissed without choosing.
//!
//! The user-side action set is server-defined -- e.g.
//! rust-analyzer: `[{ "title": "Reload Workspace" }]` after a
//! `Cargo.toml` edit. Picker UI uses the existing modal picker
//! infrastructure (P.1+) but with a synthetic source built from
//! the action list.
//!
//! **BC.8e (2026-06-24): reshaped onto the generic inbound primitive
//! (host-drained variant).** `ShowMessageRequestBus` is now a type alias for the
//! generic [`InboundBus`](lattice_mode::inbound::InboundBus) built via
//! [`make_inbound_raw`](lattice_mode::inbound::make_inbound_raw): its `send`
//! **wakes the editor**, so a server-initiated request raises the picker
//! off-keystroke (was: no wake — it only appeared on the next keypress). Like
//! apply-edit (BC.8d), this is the *host-drained* variant: the request is a
//! **deferred user choice** routed through the host picker primitive (the host
//! `drain_inbound_show_message_requests` registers the request — holding the
//! oneshot in `lsp_pending_show_message_requests` — opens the picker, and
//! resolves the oneshot from the accept / dismiss routing). That machinery is
//! irreducibly `&mut Editor` + the host picker, so the host keeps the receiver
//! (`Editor::pending_show_message_request_rx`) and the drain; the bus
//! contributes only the structural wake. No mode-owned handler, no `Effect`.

use std::sync::Arc;

use tokio::sync::oneshot;

/// The bus the supervisor fans out to each actor -- the generic inbound
/// primitive specialised to the show-message-request payload, built
/// host-drained via [`make_inbound_raw`](lattice_mode::inbound::make_inbound_raw).
/// `send` wakes the editor; the host owns the matching receiver and drains it.
/// (Was the bespoke `ShowMessageRequestBus` struct before BC.8e.)
pub type ShowMessageRequestBus = lattice_mode::inbound::InboundBus<InboundShowMessageRequest>;

/// One server-initiated `window/showMessageRequest` request,
/// ferried from the LSP actor to the host's drain.
#[derive(Debug)]
pub struct InboundShowMessageRequest {
    /// Server that sent the request.
    pub server_id: Arc<str>,
    /// Workspace root the originating actor was spawned against
    /// (B'.2). Pairs with `server_id` to form the canonical
    /// `(server_id, workspace)` instance key.
    pub workspace: Arc<std::path::Path>,
    /// Severity. Used to colour the picker prompt + bias placement.
    pub level: lsp_types::MessageType,
    /// The message text -- displayed as the picker prompt.
    pub message: String,
    /// Action labels the user picks between. Empty when the
    /// server attached no actions (degenerate -- effectively
    /// `showMessage` with a forced acknowledgement). The host
    /// auto-replies `None` for the actionless case.
    pub actions: Vec<lsp_types::MessageActionItem>,
    /// Oneshot the host fills after the user picks (or
    /// dismisses) -- held in `lsp_pending_show_message_requests`
    /// until the picker resolves.
    pub response: oneshot::Sender<ShowMessageRequestOutcome>,
}

/// Result the host reports back. `None` = user dismissed. `Some`
/// = user selected the carried action; the actor forwards this
/// verbatim to the server.
#[derive(Debug, Clone)]
pub struct ShowMessageRequestOutcome {
    pub selected: Option<lsp_types::MessageActionItem>,
}

// BC.8e: the bespoke `ShowMessageRequestBus::new()`/`dispatch()` round-trip test
// is retired — the bus is now the generic `InboundBus`, whose send/wake/dropped-
// receiver behaviour is pinned in `lattice-mode`'s inbound tests. The host-side
// drain + picker routing + deferred reply stay exercised by `lattice-ui-tui`'s
// `inject_show_message_request`-based tests against
// `Editor::drain_inbound_show_message_requests` + `finalize_show_message_request`.
