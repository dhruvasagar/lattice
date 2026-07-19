//! `auto-pair` — the first bundled plugin (AP.1 scaffold).
//!
//! ONE `wasm32-wasip2` component providing three seams (the multi-seam shape
//! proven by AP.1.0):
//!   - **grammar** — the pairing actions, fired on insert-mode chords. Each
//!     opener/closer is its OWN action because a mode keymap binding carries no
//!     args, so the action can't otherwise know which pair fired (`(` vs `[`).
//!   - **modes** — `auto-pairs-mode`, a `global` minor mode (active on document
//!     buffers) that OWNS the insert-mode keymap: the chords bind at
//!     `MinorMode(auto-pairs-mode)`, never the builtin layer (mode-ownership).
//!   - **config** — `auto-pairs-style` (`auto` | `manual`) + `auto-pairs-close-key`.
//!
//! **AP.1 registers; AP.2 implements.** The `apply-action` bodies are no-ops
//! here (the slice's exit is "loads via the loader; contributions register with
//! `SourceLayer::Plugin` provenance"). AP.2 fills the `auto` behavior — open
//! inserts the pair caret-between, close steps over a matching closer, backspace
//! deletes an empty pair — reading the buffer via the AP.0.1 `borrow<document>`
//! handle. AP.1 scaffolds the round-bracket pair + backspace; AP.2 adds the rest.

wit_bindgen::generate!({
    world: "auto-pair-plugin",
    path: "../../wit",
});

use exports::lattice::plugin_host::grammar_callbacks::Guest as GrammarCallbacks;
use lattice::plugin_host::buffer::Document;
use lattice::plugin_host::config::OptionType;
use lattice::plugin_host::modes::{
    ActivationPolicy, BindingMode, ModeCapabilities, ModeDeclaration, ModeKeymapBinding, ModeKind,
};
use lattice::plugin_host::types::{
    ActionContext, ActionSpec, Args, Effect, ExCommandContext, MotionContext, MotionResult,
    OperatorContext, Range, TextObjectContext,
};
use lattice::plugin_host::{config, grammar, modes};

struct Component;

// ── callback ids (guest-local; the host passes them back to apply_action) ─────
const CB_OPEN_ROUND: u32 = 1; // `(` → insert `()`, caret between
const CB_CLOSE_ROUND: u32 = 2; // `)` → step over a matching `)`, else insert
const CB_BACKSPACE: u32 = 3; // `<BS>` in `()` → delete both

impl Guest for Component {
    /// grammar seam — one action per opener/closer (the keymap binds each chord
    /// to the matching action; the action names are what the mode keymap resolves).
    fn register_grammar() {
        let spec = || ActionSpec {
            args_schema: Vec::new(),
        };
        grammar::register_action("auto-pair-open-round", "insert a matching )", &spec(), CB_OPEN_ROUND);
        grammar::register_action("auto-pair-close-round", "step over a matching )", &spec(), CB_CLOSE_ROUND);
        grammar::register_action("auto-pair-backspace", "delete an empty pair", &spec(), CB_BACKSPACE);
    }

    /// modes seam — `auto-pairs-mode` owns its insert-mode keymap. `global`:
    /// active on document buffers (never in `*plugin-trace*`, help, the file
    /// tree). Bindings target the plugin's OWN grammar actions by bare name —
    /// resolvable because `provides` lists `grammar` before `modes`.
    fn register_modes() {
        let bind = |chord: &str, command: &str| ModeKeymapBinding {
            binding_mode: BindingMode::Insert,
            chord: chord.to_string(),
            command: command.to_string(),
        };
        modes::register_mode(&ModeDeclaration {
            id: "auto-pairs-mode".to_string(),
            kind: ModeKind::Minor,
            activation_policy: ActivationPolicy::Global,
            capabilities: ModeCapabilities::empty(),
            keymap: vec![
                bind("(", "auto-pair-open-round"),
                bind(")", "auto-pair-close-round"),
                bind("<BS>", "auto-pair-backspace"),
            ],
        });
    }

    /// config seam — the style switch (read by the action bodies at AP.2) + the
    /// manual close key. Behavior is option-gated inside the handlers, so the
    /// keymap set stays stable across `:set auto-pairs-style=…` (no re-binding).
    fn register_options() {
        config::register_option(
            "auto-pairs-style",
            OptionType::String,
            "auto",
            "auto = complete pairs on the opening key; manual = the close key emits the pair",
        );
        config::register_option(
            "auto-pairs-close-key",
            OptionType::String,
            "<C-j>",
            "insert-mode key that closes the nearest unmatched pair (manual style)",
        );
    }
}

impl GrammarCallbacks for Component {
    /// AP.1: no-op bodies (registration is the slice's exit). AP.2 implements the
    /// `auto` behavior, reading around `ctx.cursor` via `doc`.
    fn apply_action(
        _callback: u32,
        _ctx: ActionContext,
        _doc: &Document,
    ) -> Result<Vec<Effect>, String> {
        // AP.2: match _callback → open-insert / close-skip / backspace-delete.
        Ok(vec![Effect::None])
    }

    fn apply_motion(_c: u32, _ctx: MotionContext) -> Result<MotionResult, String> {
        Err("auto-pair: no motions".into())
    }
    fn apply_operator(_c: u32, _ctx: OperatorContext) -> Result<Vec<Effect>, String> {
        Err("auto-pair: no operators".into())
    }
    fn apply_text_object(_c: u32, _ctx: TextObjectContext) -> Result<Range, String> {
        Err("auto-pair: no text objects".into())
    }
    fn parse_ex_args(_c: u32, _rest: String, _bang: bool) -> Result<Args, String> {
        Err("auto-pair: no ex-commands".into())
    }
    fn apply_ex_command(_c: u32, _ctx: ExCommandContext) -> Result<Vec<Effect>, String> {
        Err("auto-pair: no ex-commands".into())
    }
}

export!(Component);
