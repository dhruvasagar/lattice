//! `llm-mode`: the LLM layer's surface in every buffer (design §2).
//!
//! A minor mode, active on every buffer kind (`Universal`), because asking is
//! as valid from `*compilation*`, a terminal or a multibuffer as from a file
//! (design §1). It owns the `<leader>a` chords and their handler bodies; the
//! chat buffer's own chords are `llm-chat-mode`'s.
//!
//! Every chord starts with `llm.prefix` (default `<leader>a`; `<C-c>.` is a
//! common choice). Under it sit the operators (`a` ask, `A` ask through the
//! menu, `r` rewrite, `c` attach to context, each with its linewise doubled
//! form) and the commands (`l` chat, `m` menu, `C` context, `i` inspect, `R`
//! requests, `L` last answer, `k` stop). The prefix pressed alone and held
//! shows them through which-key, which reads this same layer.
//!
//! [`LlmKeys`] binds them into this mode's keymap layer, so they exist only
//! where the mode is active, and **rebinds them whenever `llm.prefix`
//! changes**: on `:set`, and when `lattice.toml` is read, which happens after
//! boot. A prefix read once at install would never see the file's value. The
//! keymap and the operator wirer are host services registered after this
//! layer installs, so the mode takes them on its first activation and binds
//! then; a change before that is picked up by that first bind.
//!
//! It also registers the `llm` modeline element (LL.14c), once, on its first
//! activation: the host's modeline service is built after the llm layer's
//! install, so the mode, reaching it through its context, is the owner that
//! can.

use lattice_mode::{
    ActionHandlerContribution, ActivationPolicy, CapabilitySet, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind,
};

use crate::llm::chat::LlmChatsHandle;
use crate::llm::operators::{
    ACTION_ASK_CONFIRMED, ACTION_ASK_CONTINUE, ACTION_ASK_SEND, LlmAsksHandle, confirmed_handler,
    continue_handler, send_handler,
};
use crate::llm::rewrite::{ACTION_REWRITE_SEND, LlmRewritesHandle};

pub struct LlmMode {
    chats: LlmChatsHandle,
    asks: LlmAsksHandle,
    rewrites: LlmRewritesHandle,
    keys: LlmKeysHandle,
    modeline_registered: std::sync::atomic::AtomicBool,
}

impl LlmMode {
    pub fn new(
        chats: LlmChatsHandle,
        asks: LlmAsksHandle,
        rewrites: LlmRewritesHandle,
        keys: LlmKeysHandle,
    ) -> Self {
        Self {
            chats,
            asks,
            rewrites,
            keys,
            modeline_registered: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn mode_id() -> ModeId {
        ModeId::new("llm-mode")
    }
}

impl Mode for LlmMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Universal
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn action_handlers(&self) -> Vec<ActionHandlerContribution> {
        vec![
            ActionHandlerContribution {
                action_name: ACTION_ASK_SEND,
                handler: send_handler(self.chats.clone(), self.asks.clone()),
            },
            ActionHandlerContribution {
                action_name: ACTION_ASK_CONTINUE,
                handler: continue_handler(self.chats.clone(), self.asks.clone()),
            },
            ActionHandlerContribution {
                action_name: ACTION_ASK_CONFIRMED,
                handler: confirmed_handler(self.chats.clone(), self.asks.clone()),
            },
            ActionHandlerContribution {
                action_name: ACTION_REWRITE_SEND,
                handler: crate::llm::rewrite::send_handler(
                    self.chats.clone(),
                    self.rewrites.clone(),
                ),
            },
        ]
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        use std::sync::atomic::Ordering;
        if !self.modeline_registered.load(Ordering::Acquire)
            && let Some(svc) = ctx.service::<lattice_mode::ModelineServiceHandle>()
        {
            crate::llm::modeline::register_element(&svc);
            self.modeline_registered.store(true, Ordering::Release);
        }
        if let (Some(keymap), Some(wirer)) = (
            ctx.service::<lattice_keymap::KeymapHandle>(),
            ctx.service::<lattice_mode::OperatorChordWirerHandle>(),
        ) {
            self.keys.attach((*keymap).clone(), (*wirer).clone());
        }
        Box::pin(async { Ok(()) })
    }
}

/// An operator under the prefix: its key, its id, and whether it reads a
/// character after its motion.
#[derive(Debug, Clone, Copy)]
pub struct PrefixOperator {
    pub key: &'static str,
    pub op: lattice_grammar::registry::OperatorId,
    pub post_motion_char: bool,
}

/// What goes under the prefix, resolved once at install.
#[derive(Debug, Clone, Default)]
pub struct PrefixBindings {
    pub operators: Vec<PrefixOperator>,
    /// `(key, command)`: commands bound in Normal mode.
    pub commands: Vec<(&'static str, lattice_grammar::CommandId)>,
}

/// The commands under the prefix, by name, in which-key's order.
pub const PREFIX_COMMANDS: [(&str, &str); 7] = [
    ("l", "llm"),
    ("m", "llm-menu"),
    ("C", "llm-context"),
    ("i", "llm-inspect"),
    ("R", "llm-requests"),
    ("L", "llm-last"),
    ("k", "llm-stop"),
];

/// Binds the llm chords under `llm.prefix`, and again whenever it changes.
pub struct LlmKeys {
    bindings: PrefixBindings,
    config: Option<std::sync::Arc<lattice_config::ConfigRegistry>>,
    handles: std::sync::OnceLock<(
        lattice_keymap::KeymapHandle,
        lattice_mode::OperatorChordWirerHandle,
    )>,
    /// The prefix bound now, so an unchanged value rebinds nothing. Held
    /// across a rebind, so two rebinds (the option changing while the mode
    /// first activates) cannot interleave.
    bound: std::sync::Mutex<Option<String>>,
}

/// Shared by `llm-mode` and the option listener.
pub type LlmKeysHandle = std::sync::Arc<LlmKeys>;

impl LlmKeys {
    pub fn new(
        bindings: PrefixBindings,
        config: Option<std::sync::Arc<lattice_config::ConfigRegistry>>,
    ) -> Self {
        Self {
            bindings,
            config,
            handles: std::sync::OnceLock::new(),
            bound: std::sync::Mutex::new(None),
        }
    }

    /// The prefix as configured now.
    pub fn prefix(&self) -> String {
        self.config
            .as_ref()
            .and_then(|c| c.get_typed::<crate::llm::options::LlmPrefix>())
            .map(|v| (*v).clone())
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| crate::llm::options::DEFAULT_PREFIX.to_string())
    }

    /// The prefix the chords are bound under now; `None` before the first
    /// activation binds them.
    pub fn bound(&self) -> Option<String> {
        self.bound.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Take the host's keymap and wirer (the first activation), and bind.
    pub fn attach(
        &self,
        keymap: lattice_keymap::KeymapHandle,
        wirer: lattice_mode::OperatorChordWirerHandle,
    ) {
        if self.handles.set((keymap, wirer)).is_ok() {
            self.rebind();
        }
    }

    /// Bind the chords under the current prefix, replacing the ones under the
    /// old one. Nothing until the handles are attached; nothing when the
    /// prefix has not changed. A prefix that does not parse keeps the old
    /// chords and says so.
    pub fn rebind(&self) {
        use lattice_keymap::{BindingMode, KeymapLayer};
        use lattice_protocol::chord::ChordPattern;
        let Some((keymap, wirer)) = self.handles.get() else {
            return;
        };
        let mut bound = self.bound.lock().unwrap_or_else(|e| e.into_inner());
        let prefix = self.prefix();
        if bound.as_deref() == Some(prefix.as_str()) {
            return;
        }
        let parse = |keys: &str| lattice_protocol::parse_chord_sequence(&wirer.expand_leader(keys));
        let Ok(head) = parse(&prefix) else {
            tracing::warn!(target: "lattice::llm", "llm.prefix: `{prefix}` is not a key sequence; the llm chords stay where they were");
            return;
        };
        let layer = KeymapLayer::MinorMode(LlmMode::mode_id());
        // The layer is this mode's alone: dropping it drops exactly the
        // chords under the old prefix.
        keymap.remove_layer(layer);
        let source = || lattice_grammar::SourceLocation::synthetic("llm-mode");
        for o in &self.bindings.operators {
            let Ok(key) = lattice_protocol::parse_chord_sequence(o.key) else {
                continue;
            };
            let chord: Vec<_> = head.iter().chain(key.iter()).cloned().collect();
            // The linewise form repeats the last key (`<leader>aaa`).
            wirer.bind_in(
                layer,
                source(),
                &chord,
                o.op,
                chord.last().cloned(),
                o.post_motion_char,
            );
        }
        for (key, command) in &self.bindings.commands {
            let Ok(key) = lattice_protocol::parse_chord_sequence(key) else {
                continue;
            };
            let path: Vec<_> = head
                .iter()
                .chain(key.iter())
                .cloned()
                .map(ChordPattern::Literal)
                .collect();
            keymap.bind(
                layer,
                BindingMode::Normal,
                &path,
                lattice_grammar::CommandInvocation::of(*command),
                source(),
            );
        }
        *bound = Some(prefix);
    }
}

/// Rebind when `llm.prefix` changes: `:set`, or the config file being read.
pub fn spawn_prefix_listener(
    keys: LlmKeysHandle,
    bus: &std::sync::Arc<lattice_runtime::EventBus>,
    runtime: &tokio::runtime::Handle,
) {
    use lattice_protocol::event::{Event, EventKind};
    use lattice_runtime::{EventFilter, SubscriptionTarget};
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
    bus.subscribe(
        EventFilter::kind(EventKind::OptionChanged),
        SubscriptionTarget::Channel(tx),
    );
    runtime.spawn(async move {
        while let Some(event) = rx.recv().await {
            if matches!(&event, Event::OptionChanged { name, .. } if name == "llm.prefix") {
                keys.rebind();
            }
        }
    });
}
