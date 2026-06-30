//! [`OptionsGenerator`] — completion source for `:set <Tab>`.
//!
//! Walks the registry on each invocation. The matcher (fuzzy by
//! default) and ranker live in `lattice-completion`; this generator
//! just produces the candidate set.
//!
//! Behaviour:
//!
//! - **Bare prefix** (no `=`): emits one candidate per registered
//!   option name plus every alternate name form the option's
//!   [`crate::OptionType::name_forms`] declares (boolean's
//!   `noNAME`).
//! - **`name=` prefix**: looks up `name` in the registry and emits
//!   `name=value` candidates for each entry the option's
//!   [`crate::ErasedOption::enumerate_values`] returns.
//!   Free-form values (integers, paths, free strings) return
//!   `None` and the candidate set is empty.
//!
//! No knowledge of `OptionKind` — the generator asks the
//! type-erased view, which delegates to the type's trait impl.
//! Adding a new option-type variant means writing one
//! [`crate::OptionType`] impl; completion picks it up
//! automatically.

use std::sync::Arc;

use lattice_completion::candidate::{CandidateData, CandidateKind, RawCandidate};
use lattice_completion::traits::{CandidateGenerator, GenerateContext};

use crate::ConfigRegistry;

pub struct OptionsGenerator {
    pub registry: Arc<ConfigRegistry>,
}

impl OptionsGenerator {
    pub fn new(registry: Arc<ConfigRegistry>) -> Self {
        Self { registry }
    }
}

impl CandidateGenerator for OptionsGenerator {
    fn generate(&self, ctx: &GenerateContext<'_>) -> Vec<RawCandidate> {
        let mut out = Vec::new();
        let prefix = ctx.prefix;
        if let Some(eq) = prefix.find('=') {
            // Value-completion mode. Look up the option to the
            // left of `=`, emit one candidate per known value.
            // Slice `3c.unify.option-doc-annotator`: emit the
            // `CandidateData::OptionValue` variant so the
            // `DocSnippetAnnotator` populates the marginalia
            // column from the type's per-value doc.
            let name = &prefix[..eq];
            if let Some(spec) = self.registry.lookup(name)
                && let Some(values) = spec.enumerate_values_with_docs()
            {
                for v in values {
                    let text = format!("{}={}", spec.name(), v.form);
                    out.push(RawCandidate {
                        text: text.clone(),
                        display: text,
                        kind: CandidateKind::Option,
                        data: CandidateData::OptionValue {
                            option_name: spec.name().to_string(),
                            value: v.form.to_string(),
                            doc: v.doc.to_string(),
                        },
                        source: None,
                        accept_action: None,
                        annotations: Vec::new(),
                        display_spans: Vec::new(),
                    });
                }
            }
            return out;
        }
        // Bare prefix: enumerate every option name + its alternate
        // forms (booleans add `noNAME`). The matcher fuzzy-filters.
        // Slice `3c.unify.option-doc-annotator`: emit the
        // `CandidateData::Option` variant so the
        // `DocSnippetAnnotator` populates the marginalia column
        // from `OptionDecl::DOC`.
        for spec in self.registry.iter() {
            // One candidate per accepted name form: the canonical
            // name, the boolean `noNAME` negation, every alias
            // (`cul` / `cursorline` for `current-line-highlight` —
            // the canonical has no `s`, so `:set curs<Tab>` can only
            // match through an alias), and the `noALIAS` negation of
            // each alias for booleans. All carry the canonical `name`
            // in `data` so accept + marginalia resolve through the
            // same spec regardless of which form the user typed.
            let mk = |text: String| RawCandidate {
                text: text.clone(),
                display: text,
                kind: CandidateKind::Option,
                data: CandidateData::Option {
                    name: spec.name().to_string(),
                    current_value: spec.get_formatted(),
                    doc: spec.doc().to_string(),
                },
                source: None,
                accept_action: None,
                annotations: Vec::new(),
                display_spans: Vec::new(),
            };
            out.push(mk(spec.name().to_string()));
            for alt in spec.name_forms() {
                out.push(mk(alt));
            }
            for alias in spec.aliases() {
                out.push(mk(alias.to_string()));
                if spec.is_bool() {
                    out.push(mk(format!("no{alias}")));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::option::Option;
    use lattice_core::Document;
    use lattice_grammar::CommandRegistry;

    #[test]
    fn bare_prefix_enumerates_option_names_and_bool_no_forms() {
        let registry = Arc::new(ConfigRegistry::new());
        registry.register(Option::<bool>::new("number", true, ""));
        registry.register(Option::<i64>::new("tabstop", 8, ""));
        let g = OptionsGenerator::new(registry);
        let doc = Document::from_text("");
        let buf = doc.buffer();
        let cmd_reg = CommandRegistry::new();
        let ctx = GenerateContext {
            prefix: "",
            buffer: buf,
            registry: &cmd_reg,
            case_sensitive: false,
        };
        let out = g.generate(&ctx);
        let names: Vec<&str> = out.iter().map(|c| c.text.as_str()).collect();
        assert!(names.contains(&"number"));
        assert!(names.contains(&"nonumber"));
        assert!(names.contains(&"tabstop"));
        // tabstop is int, no `notabstop`.
        assert!(!names.contains(&"notabstop"));
    }

    #[test]
    fn bare_prefix_enumerates_aliases_and_their_no_forms() {
        let registry = Arc::new(ConfigRegistry::new());
        // Mirror `current-line-highlight` (aliases `cul`/`cursorline`):
        // the canonical name has no `s`, so `:set curs<Tab>` is only
        // matchable through the alias candidate.
        registry.register(
            Option::<bool>::builder("current-line-highlight", false, "")
                .aliases(&["cul", "cursorline"])
                .build(),
        );
        registry.register(
            Option::<i64>::builder("tabstop", 8, "")
                .aliases(&["ts"])
                .build(),
        );
        let g = OptionsGenerator::new(registry);
        let doc = Document::from_text("");
        let buf = doc.buffer();
        let cmd_reg = CommandRegistry::new();
        let ctx = GenerateContext {
            prefix: "",
            buffer: buf,
            registry: &cmd_reg,
            case_sensitive: false,
        };
        let out = g.generate(&ctx);
        let names: Vec<&str> = out.iter().map(|c| c.text.as_str()).collect();
        // Canonical + every alias surface as name candidates.
        assert!(names.contains(&"current-line-highlight"));
        assert!(names.contains(&"cursorline"));
        assert!(names.contains(&"cul"));
        // Booleans carry the `noNAME` negation of canonical AND aliases.
        assert!(names.contains(&"nocurrent-line-highlight"));
        assert!(names.contains(&"nocursorline"));
        assert!(names.contains(&"nocul"));
        // A non-bool alias surfaces, but without a `no` form.
        assert!(names.contains(&"ts"));
        assert!(!names.contains(&"nots"));
        // Every alias candidate resolves back to the canonical spec so
        // accept + marginalia stay correct regardless of typed form.
        for c in &out {
            if c.text == "cursorline" || c.text == "cul" || c.text == "nocursorline" {
                match &c.data {
                    CandidateData::Option { name, .. } => {
                        assert_eq!(name, "current-line-highlight");
                    }
                    other => panic!("expected Option data, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn eq_prefix_emits_value_candidates_for_bool() {
        let registry = Arc::new(ConfigRegistry::new());
        registry.register(Option::<bool>::new("number", true, ""));
        let g = OptionsGenerator::new(registry);
        let doc = Document::from_text("");
        let buf = doc.buffer();
        let cmd_reg = CommandRegistry::new();
        let ctx = GenerateContext {
            prefix: "number=",
            buffer: buf,
            registry: &cmd_reg,
            case_sensitive: false,
        };
        let out = g.generate(&ctx);
        let texts: Vec<&str> = out.iter().map(|c| c.text.as_str()).collect();
        // Bool completion advertises only the canonical
        // `true`/`false` forms. The parser still accepts
        // `on`/`off`/`1`/`0`/`yes`/`no` for back-compat with
        // hand-written config files.
        assert_eq!(texts.len(), 2, "expected 2 candidates, got {texts:?}");
        assert!(texts.contains(&"number=true"));
        assert!(texts.contains(&"number=false"));
        assert!(!texts.contains(&"number=on"));
        assert!(!texts.contains(&"number=off"));
    }

    #[test]
    fn eq_prefix_emits_no_values_for_int() {
        let registry = Arc::new(ConfigRegistry::new());
        registry.register(Option::<i64>::new("tabstop", 8, ""));
        let g = OptionsGenerator::new(registry);
        let doc = Document::from_text("");
        let buf = doc.buffer();
        let cmd_reg = CommandRegistry::new();
        let ctx = GenerateContext {
            prefix: "tabstop=",
            buffer: buf,
            registry: &cmd_reg,
            case_sensitive: false,
        };
        let out = g.generate(&ctx);
        assert!(out.is_empty());
    }
}
