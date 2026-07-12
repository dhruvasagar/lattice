//! PH7.4d — the `fuzzy-finder` validation plugin.
//!
//! A `wasm32-wasip2` guest implementing the `picker-source-plugin` world that
//! replicates the native `files` picker (`lattice_picker::picker_sources::FilesSource`)
//! to prove the plugin substrate end-to-end: it walks the workspace via the
//! capability-gated `host-services` `walk` (PH7.4b) and emits one `OpenFile`
//! candidate per file. Because the host's `walk` reuses the SAME
//! `walk_files_for_picker` the native source uses, the candidate set matches
//! native by construction — the parity test formalises it.
//!
//! It is a test/bench artifact: not shipped, never registered in the editor.
//! Built-in sources stay native Rust; this exists only as the §13 exit-gate
//! validation vehicle (parity + overhead budgets).

wit_bindgen::generate!({
    world: "picker-source-plugin",
    path: "../../wit",
});

use exports::lattice::plugin_host::picker_source::{CandidatePair, Guest};
use lattice::plugin_host::host_services::walk;
use lattice::plugin_host::types::{
    CandidateData, CandidateKind, PickerAcceptOutcome, PickerContext, PickerSourceSpec,
    RawCandidate, RoutingPayload,
};

struct Component;

impl Guest for Component {
    fn spec() -> PickerSourceSpec {
        PickerSourceSpec {
            // A DISTINCT id — this is an additive custom source, never a
            // cutover of the native `files` picker (which stays native).
            id: "fuzzy-finder".to_string(),
            doc: "Recursive file picker — a WASM plugin replicating the native `files` source \
                  (Phase-7 substrate validation)."
                .to_string(),
            args_schema: Vec::new(),
            args_hint: "[root]".to_string(),
            live: false,
        }
    }

    fn init(ctx: PickerContext, args: Vec<String>) -> Result<Vec<CandidatePair>, String> {
        // Root: an explicit `args[0]`, else the projected workspace root. The
        // guest cannot read the process cwd (WASI sandbox) — it relies on the
        // host's context projection, mirroring native `files`' cwd default.
        let root = match args.first() {
            Some(p) if !p.is_empty() => p.clone(),
            _ => ctx.workspace_root.clone(),
        };
        // The capability-gated host walk (PH7.4b): absolute UTF-8 paths, the
        // SAME `walk_files_for_picker` native `files` runs, so the set matches.
        let paths = walk(&root)?;
        if paths.is_empty() {
            return Err(format!("files: no files under {root}"));
        }
        let prefix = format!("{}/", root.trim_end_matches('/'));
        let pairs = paths
            .into_iter()
            .map(|abs| {
                // Display is the path relative to the root (native strips the
                // canonical-root prefix identically), so fuzzy matching runs on
                // the path.
                let display = abs.strip_prefix(&prefix).unwrap_or(&abs).to_string();
                CandidatePair {
                    candidate: RawCandidate {
                        text: display.clone(),
                        display,
                        source: None,
                        kind: CandidateKind::Plain,
                        data: CandidateData::Plain,
                        annotations: Vec::new(),
                    },
                    routing: RoutingPayload::OpenFile(abs),
                }
            })
            .collect();
        Ok(pairs)
    }

    fn accept(_ctx: PickerContext, routing: RoutingPayload) -> Result<PickerAcceptOutcome, String> {
        match routing {
            RoutingPayload::OpenFile(path) => Ok(PickerAcceptOutcome::OpenFile(path)),
            _ => Err("files: unexpected routing payload".to_string()),
        }
    }
}

export!(Component);
