//! CM.6: the host side of the plugin-contributed compilation-parser seam.
//!
//! Design: [`compilation-mode.md`](../../../docs/dev/architecture/compilation-mode.md)
//! §5 (the parser registry as an extensibility seam) and the `error-parser`
//! WIT interface.
//!
//! A plugin implementing `error-parser-plugin` gets fed every captured
//! compilation line and returns the diagnostics it recognised. The native
//! `CompilationParser` trait is a one-method interface, so the WIT world
//! mirrors it rather than inventing a second shape for the same job.
//!
//! ## Sync, unlike most seams here
//!
//! The other async seams here exist because their work is genuinely
//! concurrent — a picker query, an event fan-out. Parsing one line is not:
//! it is a pure function of the line plus the guest's pending state, called
//! in strict arrival order by a single reader. An async call per line would
//! buy nothing and cost a suspend per line of build output.
//!
//! It is off the UI and actor threads (the compilation reader owns it), but
//! it *is* on the critical path of a fast producer, so it carries the same
//! Reflex-class budget the grammar seam uses rather than the generous
//! lifecycle default.
//!
//! ## Guest output is untrusted
//!
//! Every returned entry is validated host-side and dropped on failure, never
//! trapped on. A guest returning an empty path or a line number that cannot
//! be a line number is a buggy plugin, and a buggy plugin must cost its own
//! entries — not the build.

use lattice_protocol::error_list::{ErrorEntry, ErrorSeverity};

use crate::{Component, PluginBudget, PluginHost, PluginHostError, PluginManifest, TrustTier};

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "error-parser-plugin",
        path: "../../wit",
        // Sync exports — see the module docs. `feed` runs once per captured
        // line and must not suspend.
    });
}

use bindings::lattice::plugin_host::error_parser as wit;

/// A plugin-backed compilation parser, ready to be registered into the
/// native `ParserRegistry`.
///
/// Holds its own `Store`, so its pending multi-line state is per-instance
/// exactly like a native parser's `&mut self`.
pub struct WasmErrorParser {
    store: wasmtime::Store<crate::PluginState>,
    bindings: bindings::ErrorParserPlugin,
    plugin: String,
    /// Set once the guest traps. A trapped component is dead until reloaded
    /// (wasmtime offers no rollback), and continuing to call it would trap
    /// once per line for the rest of the build.
    poisoned: bool,
}

impl std::fmt::Debug for WasmErrorParser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmErrorParser")
            .field("plugin", &self.plugin)
            .field("poisoned", &self.poisoned)
            .finish()
    }
}

impl WasmErrorParser {
    /// Drop pending multi-line state — the start of a compilation run.
    pub fn reset(&mut self) {
        if self.poisoned {
            return;
        }
        if let Err(e) = self.bindings.call_reset(&mut self.store) {
            self.poison("reset", &e);
        }
    }

    /// Feed one line; return the entries it completed.
    ///
    /// A trap poisons the parser and yields nothing from then on: the plugin
    /// stops contributing, the build keeps streaming, and the other parsers
    /// (native and plugin) carry on.
    pub fn feed(&mut self, line: &str) -> Vec<ErrorEntry> {
        if self.poisoned {
            return Vec::new();
        }
        match self.bindings.call_feed(&mut self.store, line) {
            Ok(entries) => entries
                .into_iter()
                .filter_map(|e| validate(&self.plugin, e))
                .collect(),
            Err(e) => {
                self.poison("feed", &e);
                Vec::new()
            }
        }
    }

    fn poison(&mut self, func: &str, error: &wasmtime::Error) {
        self.poisoned = true;
        tracing::warn!(
            plugin = %self.plugin,
            func,
            error = %error,
            "error-parser plugin trapped; it will contribute nothing further this session"
        );
    }
}

/// Convert a guest entry into a host one, or drop it.
///
/// The two rejections are the ones a buggy guest actually produces: an empty
/// path (nothing to navigate to) and a line/col that would overflow when the
/// host adds its own offsets. Both are logged at `debug!` — a per-line
/// diagnostic, so `info!` would flood a noisy build (see the diagnostic-logs
/// rule in CLAUDE.md).
fn validate(plugin: &str, e: wit::Entry) -> Option<ErrorEntry> {
    if e.path.trim().is_empty() {
        tracing::debug!(
            plugin,
            "error-parser returned an entry with no path; skipping"
        );
        return None;
    }
    // A line number near u32::MAX is not a line number; it is an underflow in
    // the guest's own 1-based → 0-based conversion.
    if e.line == u32::MAX || e.col == u32::MAX {
        tracing::debug!(
            plugin,
            line = e.line,
            col = e.col,
            "error-parser returned an out-of-range position; skipping"
        );
        return None;
    }
    Some(ErrorEntry {
        path: std::path::PathBuf::from(e.path),
        line: e.line,
        col: e.col,
        severity: match e.severity {
            wit::Severity::Error => ErrorSeverity::Error,
            wit::Severity::Warning => ErrorSeverity::Warning,
            wit::Severity::Info => ErrorSeverity::Info,
            wit::Severity::Note => ErrorSeverity::Note,
        },
        message: e.message,
    })
}

impl PluginHost {
    /// CM.6: instantiate `component` as an error-parser and hand back a
    /// parser the compilation reader can drive.
    ///
    /// Uses the **sync** linker: `feed` runs once per captured line and must
    /// not suspend (see the module docs).
    pub fn spawn_error_parser(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
    ) -> Result<WasmErrorParser, PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "error-parser plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings =
            // The SYNC linker. It is named for grammar because grammar was
            // its first user, but it is the host's one sync import table —
            // sync WASI plus the sync host funcs — and instantiating against
            // a superset of a world's imports is exactly what the multi-seam
            // path already does.
            bindings::ErrorParserPlugin::instantiate(&mut store, component, &self.grammar_linker)
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;
        crate::arm_store(&mut store, budget)?;
        Ok(WasmErrorParser {
            store,
            bindings,
            plugin: manifest.id.clone(),
            poisoned: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, line: u32, col: u32) -> wit::Entry {
        wit::Entry {
            path: path.to_string(),
            line,
            col,
            severity: wit::Severity::Error,
            message: "boom".to_string(),
        }
    }

    #[test]
    fn a_well_formed_entry_converts() {
        let got = validate("p", entry("src/main.rs", 9, 4)).expect("accepted");
        assert_eq!(got.path, std::path::PathBuf::from("src/main.rs"));
        assert_eq!((got.line, got.col), (9, 4));
        assert_eq!(got.severity, ErrorSeverity::Error);
    }

    #[test]
    fn an_entry_with_no_path_is_dropped() {
        // Nothing to navigate to — it would be a quickfix row that goes
        // nowhere.
        assert!(validate("p", entry("", 1, 1)).is_none());
        assert!(validate("p", entry("   ", 1, 1)).is_none());
    }

    #[test]
    fn an_out_of_range_position_is_dropped() {
        // What a guest's own 1-based → 0-based conversion produces when it
        // underflows on line 0.
        assert!(validate("p", entry("a.rs", u32::MAX, 0)).is_none());
        assert!(validate("p", entry("a.rs", 0, u32::MAX)).is_none());
    }

    #[test]
    fn every_severity_maps() {
        for (wit_sev, host_sev) in [
            (wit::Severity::Error, ErrorSeverity::Error),
            (wit::Severity::Warning, ErrorSeverity::Warning),
            (wit::Severity::Info, ErrorSeverity::Info),
            (wit::Severity::Note, ErrorSeverity::Note),
        ] {
            let e = wit::Entry {
                severity: wit_sev,
                ..entry("a.rs", 0, 0)
            };
            assert_eq!(validate("p", e).unwrap().severity, host_sev);
        }
    }
}
