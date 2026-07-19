//! The grammar-extension guest world (PH7.7b).
//!
//! A grammar plugin implements the `grammar-plugin` world: it **imports** the
//! `grammar` register API (host-provided) and **exports** `register-grammar`
//! (the host calls it once to drive registration) + the `grammar-callbacks`
//! behaviors (`apply-*` / `parse-ex-args`, dispatched by callback-id). This
//! module holds the **fourth `bindgen!`** (after `plugin`, `picker-source-plugin`,
//! `completion-source-plugin`) for that world — the two-bindgen-with-shared-types
//! trick (`with:` points `types` at the `plugin` world's generated module so a
//! crossed value is the SAME Rust type `WitBoundary` round-trips, the PH7.3d
//! precedent).
//!
//! **Fully synchronous (the PH7.7 fork).** Unlike picker/completion, a grammar
//! `apply` resolves on the keystroke path — a motion must return inline to
//! compose with its operator (async would break operator∘motion atomicity +
//! dot-repeat/macros). So the `bindgen!` sets **no** `exports: { default: async }`:
//! the `register-grammar` + `grammar-callbacks` exports are sync-callable from the
//! dispatch thread, bounded by fuel + epoch (a Reflex-class budget, PH7.7c). No
//! actor task — the sync trampoline calls the guest directly (PH7.7c).
//!
//! Registration flow: the host calls the guest's `register-grammar` export; the
//! guest calls the imported `register-*` host functions; those record into the
//! Store's [`GrammarContributions`] (via the `grammar::Host` impl on
//! `PluginState`, `lib.rs`); after the export returns, the host drains the
//! contributions and builds native `*Spec`s with trampoline `apply`s (PH7.7c).

use crate::lattice::plugin_host::types::{
    ActionSpec as WitActionSpec, ExCommandSpec as WitExCommandSpec, MotionSpec as WitMotionSpec,
    OperatorSpec as WitOperatorSpec, TextObjectSpec as WitTextObjectSpec,
};

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "grammar-plugin",
        path: "../../wit",
        // No `exports: { default: async }` — the grammar seam is SYNCHRONOUS
        // (the PH7.7 fork): `register-grammar` + the `grammar-callbacks` `apply-*`
        // exports are sync-callable from the dispatch thread, bounded by
        // fuel/epoch. The `grammar` import's `register-*` host funcs are sync too
        // (they only record into `PluginState`; they cannot trap).
        with: {
            // Reuse the `plugin` world's generated mirrors so a value crossing
            // here is the same Rust type `WitBoundary` round-trips.
            "lattice:plugin-host/types": crate::lattice::plugin_host::types,
            // AP.0.1: `apply-action` takes a `borrow<document>`. Map the
            // host-owned resource to `DocumentResource` (the backing built +
            // unit-tested at PH7.3c) so bindgen emits the `HostDocument` trait
            // the host implements + the sync-linker `add_to_linker`.
            "lattice:plugin-host/buffer.document": crate::buffer::DocumentResource,
        },
    });
}

/// One grammar contribution a plugin declared through the `register-*` API,
/// recorded verbatim (name + doc + WIT spec metadata + the guest's callback id).
/// The host drains these after `register-grammar` returns and builds a native
/// `*Spec` with a trampoline `apply`/`parse_args` stamped `SourceLayer::Plugin`
/// (PH7.7c). The WIT spec is held as-is — its scalar fields convert at drain time
/// via `boundary_grammar` (`LatencyClass`/`SurfaceForm`) + the existing `ArgSpec`
/// mirror; the native `*Spec` cannot be built until the trampoline closure exists.
pub enum RecordedContribution {
    Motion {
        name: String,
        doc: String,
        spec: WitMotionSpec,
        /// Guest-chosen id the host passes to `apply-motion` on dispatch.
        callback: u32,
    },
    Operator {
        name: String,
        doc: String,
        spec: WitOperatorSpec,
        callback: u32,
    },
    TextObject {
        name: String,
        doc: String,
        spec: WitTextObjectSpec,
        callback: u32,
    },
    Action {
        name: String,
        doc: String,
        spec: WitActionSpec,
        callback: u32,
    },
    ExCommand {
        name: String,
        doc: String,
        spec: WitExCommandSpec,
        /// `parse-ex-args` callback id.
        parse_callback: u32,
        /// `apply-ex-command` callback id.
        apply_callback: u32,
    },
}

impl RecordedContribution {
    /// The contribution's registered name (`register_*`'s `name` arg).
    pub fn name(&self) -> &str {
        match self {
            RecordedContribution::Motion { name, .. }
            | RecordedContribution::Operator { name, .. }
            | RecordedContribution::TextObject { name, .. }
            | RecordedContribution::Action { name, .. }
            | RecordedContribution::ExCommand { name, .. } => name,
        }
    }
}

/// The per-plugin accumulator the `grammar::Host` impl records into during
/// `register-grammar` (`lib.rs`). Held in `PluginState`; drained by the host
/// after the registration export returns (PH7.7c). The `record_*` methods are
/// the sync host-func bodies (they only push — they cannot trap), factored here
/// (the `host_services::walk_within_grant` precedent) so the recording logic is
/// unit-testable without a `PluginState` / guest.
#[derive(Default)]
pub struct GrammarContributions {
    recorded: Vec<RecordedContribution>,
}

impl GrammarContributions {
    /// Record a motion contribution (the `grammar.register-motion` body).
    pub fn record_motion(&mut self, name: String, doc: String, spec: WitMotionSpec, callback: u32) {
        self.recorded.push(RecordedContribution::Motion {
            name,
            doc,
            spec,
            callback,
        });
    }

    /// Record an operator contribution (`grammar.register-operator`).
    pub fn record_operator(
        &mut self,
        name: String,
        doc: String,
        spec: WitOperatorSpec,
        callback: u32,
    ) {
        self.recorded.push(RecordedContribution::Operator {
            name,
            doc,
            spec,
            callback,
        });
    }

    /// Record a text-object contribution (`grammar.register-text-object`).
    pub fn record_text_object(
        &mut self,
        name: String,
        doc: String,
        spec: WitTextObjectSpec,
        callback: u32,
    ) {
        self.recorded.push(RecordedContribution::TextObject {
            name,
            doc,
            spec,
            callback,
        });
    }

    /// Record an action contribution (`grammar.register-action`).
    pub fn record_action(&mut self, name: String, doc: String, spec: WitActionSpec, callback: u32) {
        self.recorded.push(RecordedContribution::Action {
            name,
            doc,
            spec,
            callback,
        });
    }

    /// Record an ex-command contribution (`grammar.register-ex-command`). Carries
    /// two callbacks — `parse-ex-args` + `apply-ex-command`.
    pub fn record_ex_command(
        &mut self,
        name: String,
        doc: String,
        spec: WitExCommandSpec,
        parse_callback: u32,
        apply_callback: u32,
    ) {
        self.recorded.push(RecordedContribution::ExCommand {
            name,
            doc,
            spec,
            parse_callback,
            apply_callback,
        });
    }

    /// How many contributions were recorded.
    pub fn len(&self) -> usize {
        self.recorded.len()
    }

    /// True when the plugin registered no grammar (the degenerate case).
    pub fn is_empty(&self) -> bool {
        self.recorded.is_empty()
    }

    /// Drain the recorded contributions, leaving the accumulator empty. Called by
    /// the host after `register-grammar` returns (PH7.7c) to build native specs.
    pub fn take(&mut self) -> Vec<RecordedContribution> {
        std::mem::take(&mut self.recorded)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    fn arg_schema() -> Vec<crate::lattice::plugin_host::types::ArgSpec> {
        Vec::new()
    }

    #[test]
    fn records_each_kind_and_preserves_callback_ids() {
        let mut g = GrammarContributions::default();
        assert!(g.is_empty());

        g.record_motion(
            "ArrowDown".into(),
            "jump down".into(),
            WitMotionSpec {
                jump: true,
                exclusive: false,
                args_schema: arg_schema(),
            },
            10,
        );
        g.record_operator(
            "surround".into(),
            "wrap".into(),
            WitOperatorSpec {
                repeatable: true,
                args_schema: arg_schema(),
                blockwise_per_row: false,
            },
            20,
        );
        g.record_text_object(
            "entire".into(),
            "whole buffer".into(),
            WitTextObjectSpec {
                args_schema: arg_schema(),
            },
            30,
        );
        g.record_action(
            "greet".into(),
            "say hi".into(),
            WitActionSpec {
                args_schema: arg_schema(),
            },
            40,
        );
        g.record_ex_command(
            "Hello".into(),
            "greet cmd".into(),
            WitExCommandSpec {
                latency_class: crate::lattice::plugin_host::types::LatencyClass::Reflex,
                accepts_bang: true,
                accepts_range: false,
                args_schema: arg_schema(),
                surface_form: crate::lattice::plugin_host::types::SurfaceForm::Keyword,
            },
            50,
            51,
        );

        assert_eq!(g.len(), 5);
        let drained = g.take();
        assert!(g.is_empty(), "take() leaves the accumulator empty");
        assert_eq!(drained.len(), 5);

        // Provenance the trampoline (PH7.7c) reads: name + callback id per kind.
        match &drained[0] {
            RecordedContribution::Motion { name, callback, .. } => {
                assert_eq!(name, "ArrowDown");
                assert_eq!(*callback, 10);
            }
            _ => panic!("expected Motion first"),
        }
        match &drained[4] {
            RecordedContribution::ExCommand {
                name,
                parse_callback,
                apply_callback,
                ..
            } => {
                assert_eq!(name, "Hello");
                assert_eq!(*parse_callback, 50);
                assert_eq!(*apply_callback, 51);
            }
            _ => panic!("expected ExCommand last"),
        }
        assert_eq!(drained[1].name(), "surround");
        assert_eq!(drained[2].name(), "entire");
        assert_eq!(drained[3].name(), "greet");
    }
}
