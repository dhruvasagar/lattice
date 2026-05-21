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
            out.push(RawCandidate {
                text: spec.name().to_string(),
                display: spec.name().to_string(),
                kind: CandidateKind::Option,
                data: CandidateData::Option {
                    name: spec.name().to_string(),
                    current_value: spec.get_formatted(),
                    doc: spec.doc().to_string(),
                },
                source: None,
            });
            for alt in spec.name_forms() {
                out.push(RawCandidate {
                    text: alt.clone(),
                    display: alt,
                    kind: CandidateKind::Option,
                    data: CandidateData::Option {
                        name: spec.name().to_string(),
                        current_value: spec.get_formatted(),
                        doc: spec.doc().to_string(),
                    },
                    source: None,
                });
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
