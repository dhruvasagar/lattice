//! PO.4.1 — render a [`PluginTraceRecord`] into one buffer line. Pure +
//! presentation-only: the tracer hands over structured records (design §5) and
//! the view owns how they look, so filters/severity can key on the fields while
//! the line stays human-scannable.
//!
//! Shape: `{level} [plugin:{id}] {seam} {call} → {outcome}`, e.g.
//!   `debug [plugin:3] grammar apply-motion → ok 34µs`
//!   `debug [plugin:3] grammar apply-operator → ok 12µs, 1.2k fuel`
//!   `warn  [plugin:3] grammar apply-motion → guest-err`
//!   `error [plugin:3] grammar apply-motion → trap(fuel)`
//!   `error [plugin:3] logging log → denied(fs)`

use lattice_plugin_host::{Direction, PluginTraceRecord, TraceLevel, TraceOutcome};

/// The shared synthetic buffer name + the major mode that owns the trace views.
pub const SHARED_BUFFER_NAME: &str = "*plugin-trace*";
pub const TRACE_MODE_ID: &str = "plugin-trace-mode";

/// The per-plugin buffer name for `plugin` (`*plugin-trace:<name>*`) — the
/// manager `t` drill-in target (PO.4.2). Single-sources the naming scheme with
/// [`parse_per_plugin_name`] so the manager (producer) and the mode (consumer)
/// can't drift.
pub fn per_plugin_buffer_name(plugin: &str) -> String {
    format!("*plugin-trace:{plugin}*")
}

/// The plugin name inside a `*plugin-trace:<name>*` buffer name, or `None` for
/// the shared `*plugin-trace*` (or any non-trace name). Inverse of
/// [`per_plugin_buffer_name`].
pub fn parse_per_plugin_name(buffer_name: &str) -> Option<&str> {
    buffer_name
        .strip_prefix("*plugin-trace:")
        .and_then(|rest| rest.strip_suffix('*'))
}

/// The level tag, right-padded to `error`/`trace` width so lines align in the
/// column. The label itself is single-sourced on `TraceLevel::as_str`.
fn level_tag(level: TraceLevel) -> String {
    format!("{:<5}", level.as_str())
}

/// Compact a fuel count (`1234` → `1.2k`, `56` → `56`). Trace lines want the
/// magnitude, not the exact figure.
fn fuel_short(fuel: u64) -> String {
    if fuel >= 1000 {
        format!("{:.1}k", fuel as f64 / 1000.0)
    } else {
        fuel.to_string()
    }
}

/// The `→ …` outcome cell. Presentation owns the one convention PO.3 folded into
/// the data model: a `Warn`-level `Ok` is a guest-returned `err` (the grammar
/// seam's graceful no-op), rendered `guest-err` rather than `ok` so the line
/// reads truthfully.
fn outcome_cell(level: TraceLevel, outcome: &TraceOutcome) -> String {
    match outcome {
        TraceOutcome::Trap { kind, .. } => format!("trap({kind})"),
        TraceOutcome::Denied { capability } => format!("denied({capability})"),
        TraceOutcome::Ok { .. } if level == TraceLevel::Warn => "guest-err".to_string(),
        TraceOutcome::Ok { micros, fuel_delta } => {
            let mut s = format!("ok {micros}µs");
            if *fuel_delta > 0 {
                s.push_str(&format!(", {} fuel", fuel_short(*fuel_delta)));
            }
            s
        }
    }
}

/// The direction glyph: guest→host (a host import the guest called) vs host→guest
/// (a guest export the host drove). Kept subtle — most records are exports.
fn direction_arrow(direction: Direction) -> &'static str {
    match direction {
        Direction::HostImport => "«", // guest → host
        Direction::GuestExport => "»", // host → guest
    }
}

/// Format one record into a single line (no trailing newline — the drain joins
/// with `\n`).
pub fn format_trace_line(record: &PluginTraceRecord) -> String {
    format!(
        "{lvl} [plugin:{id}] {seam} {dir}{call} → {outcome}",
        lvl = level_tag(record.level),
        id = record.plugin,
        seam = record.seam.as_str(),
        dir = direction_arrow(record.direction),
        call = record.call,
        outcome = outcome_cell(record.level, &record.outcome),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_plugin_host::PluginSeam;

    fn rec(level: TraceLevel, outcome: TraceOutcome) -> PluginTraceRecord {
        PluginTraceRecord {
            plugin: 3,
            seam: PluginSeam::Grammar,
            direction: Direction::GuestExport,
            call: "apply-motion".into(),
            level,
            outcome,
            detail: None,
        }
    }

    #[test]
    fn a_success_shows_level_plugin_seam_call_and_timing() {
        let line = format_trace_line(&rec(
            TraceLevel::Debug,
            TraceOutcome::Ok {
                micros: 34,
                fuel_delta: 0,
            },
        ));
        assert_eq!(line, "debug [plugin:3] grammar »apply-motion → ok 34µs");
    }

    #[test]
    fn fuel_is_appended_and_shortened_when_nonzero() {
        let line = format_trace_line(&rec(
            TraceLevel::Debug,
            TraceOutcome::Ok {
                micros: 12,
                fuel_delta: 1234,
            },
        ));
        assert!(line.ends_with("→ ok 12µs, 1.2k fuel"), "got {line}");
    }

    #[test]
    fn a_warn_ok_reads_as_guest_err() {
        let line = format_trace_line(&rec(
            TraceLevel::Warn,
            TraceOutcome::Ok {
                micros: 0,
                fuel_delta: 0,
            },
        ));
        assert!(line.starts_with("warn "), "got {line}");
        assert!(line.ends_with("→ guest-err"), "got {line}");
    }

    #[test]
    fn a_trap_shows_its_kind() {
        let line = format_trace_line(&rec(
            TraceLevel::Error,
            TraceOutcome::Trap {
                kind: "fuel".to_string(),
                func: "apply-motion".to_string(),
            },
        ));
        assert!(line.starts_with("error "), "got {line}");
        assert!(line.ends_with("→ trap(fuel)"), "got {line}");
    }

    #[test]
    fn a_denied_shows_the_capability() {
        let mut r = rec(
            TraceLevel::Error,
            TraceOutcome::Denied {
                capability: "fs".to_string(),
            },
        );
        r.direction = Direction::HostImport;
        let line = format_trace_line(&r);
        assert!(line.contains("«apply-motion"), "guest→host glyph, got {line}");
        assert!(line.ends_with("→ denied(fs)"), "got {line}");
    }

    #[test]
    fn per_plugin_name_round_trips() {
        let name = per_plugin_buffer_name("fuzzy-finder");
        assert_eq!(name, "*plugin-trace:fuzzy-finder*");
        assert_eq!(parse_per_plugin_name(&name), Some("fuzzy-finder"));
        // The shared name (and anything else) has no per-plugin id.
        assert_eq!(parse_per_plugin_name(SHARED_BUFFER_NAME), None);
        assert_eq!(parse_per_plugin_name("*scratch*"), None);
    }
}
