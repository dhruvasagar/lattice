// `linkme`'s distributed slices use `link_section` to aggregate items at link
// time. The `options!` macro expansion below emits such a declaration; allow the
// workspace's `unsafe_code = "deny"` lint locally with the same safety rationale
// documented in `option_decl.rs`, `group.rs`, and `core_options.rs`.
#![allow(unsafe_code)]

//! Plugin observability options (`plugin.*`). PO.4.3: `plugin.trace-level` sets
//! the global default boundary-trace verbosity the host's `PluginTracer` gates on
//! (design `docs/dev/architecture/plugin-observability.md` §7). The loader
//! observes `Event::OptionChanged` and pushes the new level into the tracer live
//! (PO.3's per-plugin gate republish reaches the hot path on the next keystroke).
//!
//! The value type mirrors `lattice_plugin_host::TraceLevel`, duplicated here
//! because `lattice-config` is foundational and cannot depend on the plugin host;
//! the loader bridges the two by the option's string form (the labels match).

use crate::option_type::{EnumeratedValue, OptionType};

/// `plugin.trace-level` — the global default plugin boundary-trace verbosity.
/// Ordered least→most verbose; a record is kept when its level ≤ this gate.
/// `info` (the default) drops per-call traces — authors opt a plugin up to
/// `debug` / `trace` (globally here, or per-plugin from the `:plugins` view).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PluginTraceLevel {
    /// Silence every plugin's trace entirely.
    Off,
    Error,
    Warn,
    /// The default: crash/lifecycle signal only, no per-call traces.
    #[default]
    Info,
    Debug,
    Trace,
}

impl PluginTraceLevel {
    pub fn label(&self) -> &'static str {
        match self {
            PluginTraceLevel::Off => "off",
            PluginTraceLevel::Error => "error",
            PluginTraceLevel::Warn => "warn",
            PluginTraceLevel::Info => "info",
            PluginTraceLevel::Debug => "debug",
            PluginTraceLevel::Trace => "trace",
        }
    }

    pub fn doc(&self) -> &'static str {
        match self {
            PluginTraceLevel::Off => "Silence all plugin traces",
            PluginTraceLevel::Error => "Traps only",
            PluginTraceLevel::Warn => "Guest errors and traps",
            PluginTraceLevel::Info => "Crash/lifecycle only (no per-call traces)",
            PluginTraceLevel::Debug => "Every host↔guest call, timed",
            PluginTraceLevel::Trace => "Every call with argument/result detail",
        }
    }

    pub fn all() -> [PluginTraceLevel; 6] {
        [
            PluginTraceLevel::Off,
            PluginTraceLevel::Error,
            PluginTraceLevel::Warn,
            PluginTraceLevel::Info,
            PluginTraceLevel::Debug,
            PluginTraceLevel::Trace,
        ]
    }

    pub fn parse_label(s: &str) -> Result<Self, String> {
        match s {
            "off" => Ok(PluginTraceLevel::Off),
            "error" => Ok(PluginTraceLevel::Error),
            "warn" => Ok(PluginTraceLevel::Warn),
            "info" => Ok(PluginTraceLevel::Info),
            "debug" => Ok(PluginTraceLevel::Debug),
            "trace" => Ok(PluginTraceLevel::Trace),
            other => Err(format!(
                "plugin.trace-level: expected `off`, `error`, `warn`, `info`, `debug`, or `trace`, got `{other}`"
            )),
        }
    }
}

impl OptionType for PluginTraceLevel {
    fn parse(s: &str) -> Result<Self, String> {
        PluginTraceLevel::parse_label(s)
    }
    fn format(&self) -> String {
        self.label().to_string()
    }
    fn type_label() -> &'static str {
        "plugin-trace-level"
    }
    fn enumerate() -> Option<Vec<&'static str>> {
        Some(PluginTraceLevel::all().iter().map(|v| v.label()).collect())
    }
    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        Some(
            PluginTraceLevel::all()
                .iter()
                .map(|v| EnumeratedValue {
                    form: v.label(),
                    doc: v.doc(),
                })
                .collect(),
        )
    }
}

crate::options! {
    group = crate::Plugin;

    /// Global default plugin boundary-trace verbosity. `off` / `error` / `warn` /
    /// `info` (default) / `debug` / `trace`. `info` and below carry only
    /// crash/lifecycle signal — no per-call traces (the keystroke hot path stays
    /// free). Raise to `debug` to trace every host↔guest call; per-plugin
    /// overrides live in the `:plugins` view (`T`).
    #[name("plugin.trace-level")]
    pub PluginTraceLevelOption: PluginTraceLevel = PluginTraceLevel::Info;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use crate::{ConfigRegistry, PluginTraceLevel, PluginTraceLevelOption};

    fn reg() -> ConfigRegistry {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        r
    }

    #[test]
    fn default_is_info() {
        let r = reg();
        assert_eq!(
            *r.get_typed::<PluginTraceLevelOption>().unwrap(),
            PluginTraceLevel::Info
        );
    }

    #[test]
    fn set_debug_parses() {
        let r = reg();
        r.parse_and_set_command("plugin.trace-level=debug").unwrap();
        assert_eq!(
            *r.get_typed::<PluginTraceLevelOption>().unwrap(),
            PluginTraceLevel::Debug
        );
    }

    #[test]
    fn bad_value_errors() {
        let r = reg();
        assert!(r.parse_and_set_command("plugin.trace-level=loud").is_err());
    }

    #[test]
    fn label_round_trips_every_level() {
        for lvl in PluginTraceLevel::all() {
            assert_eq!(PluginTraceLevel::parse_label(lvl.label()), Ok(lvl));
        }
    }
}
