//! PH7.6 completion fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `completion-source-plugin`
//! world. It produces a fixed keyword set (ignoring the query prefix — matching
//! is NATIVE, the option-A design), each candidate carrying its data via the
//! `candidate-data.extension` hatch (the plugin-payload path). The host drives
//! `generate` through the `CompletionActor` + `WasmCompletionSource` adapter and
//! runs the native `match_and_rank` over the result.

wit_bindgen::generate!({
    world: "completion-source-plugin",
    path: "../../../../../wit",
});

use exports::lattice::plugin_host::completion_source::Guest;
use lattice::plugin_host::types::{
    CandidateData, CandidateExtension, CandidateKind, CompletionSourceSpec, GenerateContext,
    RawCandidate,
};

struct Component;

impl Guest for Component {
    fn spec() -> CompletionSourceSpec {
        CompletionSourceSpec {
            id: "keywords".to_string(),
            doc: "Fixture keyword completion source (PH7.6 substrate validation).".to_string(),
            // Identifier completion — the default. The phrase-source path is
            // covered by org-roam's own tests.
            accepts_non_word_query: false,
        }
    }

    fn generate(_ctx: GenerateContext) -> Result<Vec<RawCandidate>, String> {
        // Return the full keyword set; the native matcher filters against the
        // query prefix (matching stays native — option A). Each candidate uses
        // the `extension` data hatch (kind-id 1, empty payload) — the plugin
        // candidate-data path.
        Ok(["alpha", "alphabet", "beta", "gamma"]
            .iter()
            .map(|w| RawCandidate {
                insert_text: None,
                text: w.to_string(),
                display: w.to_string(),
                source: Some("keywords".to_string()),
                kind: CandidateKind::Plain,
                data: CandidateData::Extension(CandidateExtension {
                    kind_id: 1,
                    payload: Vec::new(),
                }),
                annotations: Vec::new(),
                display_spans: Vec::new(),
            })
            .collect())
    }
}

export!(Component);
