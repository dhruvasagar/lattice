//! PL8.H.2 — render a `Vec<PluginStatus>` (the loader's read model) into the
//! `*plugins*` buffer's text. Pure + presentation-only: the loader hands over
//! structured `PluginStatus` (typed capabilities + health), and the view owns
//! how it looks. No I/O, no allocation on any hot path (this runs off-thread on
//! mode activation / a crash re-render).

use lattice_plugin_host::{Capability, TrustTier};
use lattice_plugin_loader::{FailedLoad, PluginHealth, PluginStatus};

/// The synthetic buffer's user-facing name and the major mode that owns it.
pub const PLUGINS_BUFFER_NAME: &str = "*plugins*";
pub const PLUGINS_MODE_ID: &str = "plugins-mode";

/// Buffer lines the header occupies before the first plugin row: the title
/// (`# Plugins (N loaded)`), a blank line, and the column header. The
/// interactivity layer (PL8.H.3) maps `cursor.line - HEADER_LINES` → the plugin
/// at that index (render order == `plugin_status()` order), so this MUST match
/// the header `render_status` emits — pinned by `header_occupies_exactly_three_lines`.
pub const HEADER_LINES: usize = 3;

/// Short health label for the status column. Kept terse; the crash provenance
/// (which export trapped, and how) trails the row so the column stays narrow.
fn health_label(health: &PluginHealth) -> &'static str {
    match health {
        PluginHealth::Healthy => "ok",
        PluginHealth::Quarantined { .. } => "quarantined",
    }
}

/// The trust tier as the wire word used in the manifest / `:plugin-load` docs.
fn tier_label(tier: TrustTier) -> &'static str {
    match tier {
        TrustTier::Bundled => "bundled",
        TrustTier::UserInstalled => "user-installed",
    }
}

/// The capability cell: granted capabilities in wire form (`Capability`'s
/// `Display`), then any denied ones in a trailing `(denied: …)` note. Empty
/// grant renders as `—` so the column never looks blank-by-accident.
fn caps_cell(granted: &[Capability], denied: &[Capability]) -> String {
    let join = |caps: &[Capability]| {
        caps.iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let mut cell = if granted.is_empty() {
        "—".to_string()
    } else {
        join(granted)
    };
    if !denied.is_empty() {
        cell.push_str(&format!("  (denied: {})", join(denied)));
    }
    cell
}

/// The crash-provenance suffix for a quarantined plugin (`[trap: <kind> in
/// <func>]`), empty for a healthy one — trails the row so a glance down the
/// HEALTH column still reads cleanly.
fn crash_suffix(health: &PluginHealth) -> String {
    match health {
        PluginHealth::Healthy => String::new(),
        PluginHealth::Quarantined { func, kind } => format!("  [trap: {kind} in {func}]"),
    }
}

/// Render the whole `*plugins*` buffer text from the current status snapshot.
/// Column widths adapt to the content (name + tier), so the table stays aligned
/// whether one plugin is loaded or fifty. The empty state is an explicit line,
/// not a bare header.
pub fn render_status(plugins: &[PluginStatus]) -> String {
    render_status_with_failures(plugins, &[])
}

/// WT.4: [`render_status`] plus a trailing section for plugins that tried to
/// load and could not.
///
/// **Trailing, and deliberately so.** The interactivity layer maps
/// `cursor.line - HEADER_LINES` into the loaded-plugin list, so anything
/// inserted above or between the rows would put chords on the wrong plugin.
/// Failed entries have no host id to unload or reload anyway — they are a
/// report, not a row you can act on — so appending them costs the mapping
/// nothing.
///
/// Why it exists at all: a plugin that failed to load is otherwise
/// indistinguishable from one that was never installed. That is precisely what
/// made the reported failure take a debugging session — org was absent, and
/// absent looks the same either way.
pub fn render_status_with_failures(plugins: &[PluginStatus], failed: &[FailedLoad]) -> String {
    let mut out = format!("# Plugins ({} loaded)\n\n", plugins.len());
    if plugins.is_empty() {
        out.push_str("No plugins are loaded. Load one with `:plugin-load <path>`.\n");
        out.push_str(&failures_section(failed));
        return out;
    }

    // Column widths: adapt name + tier to their content (health is fixed-vocab).
    let name_w = plugins
        .iter()
        .map(|p| p.name.len())
        .chain(std::iter::once("NAME".len()))
        .max()
        .unwrap_or(4);
    let health_w = "quarantined".len();
    let tier_w = plugins
        .iter()
        .map(|p| tier_label(p.tier).len())
        .chain(std::iter::once("TIER".len()))
        .max()
        .unwrap_or(7);
    // PM.8a: SOURCE + BUILD. They sit between TIER and CAPABILITIES rather
    // than at the end because CAPABILITIES is the one variable-length cell
    // (it trails a `(denied: …)` note), so anything after it would not line
    // up down the table.
    let source_w = plugins
        .iter()
        .map(|p| p.source.label().len())
        .chain(std::iter::once("SOURCE".len()))
        .max()
        .unwrap_or(6);
    let build_w = "build-failed".len();

    out.push_str(&format!(
        "  {:<name_w$}  {:<health_w$}  {:<tier_w$}  {:<source_w$}  {:<build_w$}  CAPABILITIES\n",
        "NAME", "HEALTH", "TIER", "SOURCE", "BUILD",
    ));
    for p in plugins {
        out.push_str(&format!(
            "  {:<name_w$}  {:<health_w$}  {:<tier_w$}  {:<source_w$}  {:<build_w$}  {}{}\n",
            p.name,
            health_label(&p.health),
            tier_label(p.tier),
            p.source.label(),
            p.build.label(),
            caps_cell(&p.granted, &p.denied),
            crash_suffix(&p.health),
        ));
    }
    out.push_str(&failures_section(failed));
    out
}

/// The trailing "failed to load" block, empty when nothing failed.
///
/// One entry over two lines — name and directory, then the reason indented
/// under it — rather than a table column. A load error is a sentence (a wasm
/// trap, a missing import, a manifest complaint), and squeezing sentences into a
/// fixed-width cell is how the useful half gets truncated away.
fn failures_section(failed: &[FailedLoad]) -> String {
    if failed.is_empty() {
        return String::new();
    }
    let mut out = format!("\n## Failed to load ({})\n\n", failed.len());
    for f in failed {
        out.push_str(&format!("  {}  ({})\n", f.name, f.dir.display()));
        out.push_str(&format!("      {}\n", f.error));
    }
    out.push_str("\nIf the plugin API changed, run `lattice --wit-sync` and restart to rebuild.\n");
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_plugin_loader::{BuildState, SourceRecord};

    fn status(name: &str, tier: TrustTier, health: PluginHealth) -> PluginStatus {
        PluginStatus {
            id: 1,
            name: name.to_string(),
            tier,
            granted: vec![],
            denied: vec![],
            health,
            source: SourceRecord::Unknown,
            build: BuildState::NotBuilt,
        }
    }

    /// A row with a known source + build state, for the PM.8a column tests.
    fn sourced(name: &str, source: SourceRecord, build: BuildState) -> PluginStatus {
        PluginStatus {
            source,
            build,
            ..status(name, TrustTier::UserInstalled, PluginHealth::Healthy)
        }
    }

    #[test]
    fn the_source_and_build_columns_render_their_labels() {
        let out = render_status(&[
            sourced(
                "from-git",
                SourceRecord::Git {
                    url: "https://example.invalid/p.git".into(),
                    rev: Some("abc1234def".into()),
                },
                BuildState::Stale,
            ),
            sourced("shipped", SourceRecord::Bundled, BuildState::NotBuilt),
        ]);
        assert!(out.contains("SOURCE"), "the header names the column: {out}");
        assert!(out.contains("BUILD"));
        assert!(out.contains("git@abc1234"), "short rev in the cell: {out}");
        assert!(out.contains("stale"));
        assert!(out.contains("bundled"));
    }

    fn failure(name: &str, error: &str) -> FailedLoad {
        FailedLoad {
            name: name.to_string(),
            dir: std::path::PathBuf::from(format!("/plugins/{name}")),
            error: error.to_string(),
        }
    }

    /// WT.4: the whole point. A plugin that failed to load must be visibly
    /// *present and broken*, not absent — absent is indistinguishable from
    /// never-installed, which is what made the reported failure invisible.
    #[test]
    fn a_failed_plugin_is_named_with_its_reason_and_its_directory() {
        let out = render_status_with_failures(
            &[status("fine", TrustTier::Bundled, PluginHealth::Healthy)],
            &[failure(
                "org",
                "plugin runtime error: unknown import `logging`",
            )],
        );
        assert!(out.contains("Failed to load (1)"), "{out}");
        assert!(out.contains("org"), "the plugin is named: {out}");
        assert!(
            out.contains("unknown import `logging`"),
            "the reason survives in full: {out}"
        );
        assert!(
            out.contains("/plugins/org"),
            "and which copy on disk to go and look at: {out}"
        );
        assert!(out.contains("--wit-sync"), "with the repair to try: {out}");
    }

    /// The failures trail the table. The interactivity layer maps
    /// `cursor.line - HEADER_LINES` into the loaded list, so a section inserted
    /// above or between rows would silently put `u` / `r` / `b` on the wrong
    /// plugin.
    #[test]
    fn failures_render_after_every_loaded_row() {
        let out = render_status_with_failures(
            &[
                status("aaa", TrustTier::Bundled, PluginHealth::Healthy),
                status("zzz", TrustTier::Bundled, PluginHealth::Healthy),
            ],
            &[failure("broken", "nope")],
        );
        let lines: Vec<&str> = out.lines().collect();
        let last_row = lines.iter().rposition(|l| l.contains("zzz")).unwrap();
        let section = lines
            .iter()
            .position(|l| l.contains("Failed to load"))
            .unwrap();
        assert!(section > last_row, "failures come last:\n{out}");
        assert!(
            lines[HEADER_LINES].contains("aaa"),
            "and the first row still lands at HEADER_LINES: {out}"
        );
    }

    /// Nothing failed, nothing said. A permanent empty "Failed to load (0)"
    /// heading would train the eye to skip the section that matters.
    #[test]
    fn no_failures_renders_no_section() {
        let out = render_status_with_failures(
            &[status("fine", TrustTier::Bundled, PluginHealth::Healthy)],
            &[],
        );
        assert!(!out.contains("Failed to load"), "{out}");
        assert_eq!(
            out,
            render_status(&[status("fine", TrustTier::Bundled, PluginHealth::Healthy)]),
            "and it is byte-identical to the no-failures renderer"
        );
    }

    /// The empty-loaded-set case still reports failures — and this is the
    /// combination the reported failure actually produced: init.rs died, so
    /// nothing it required installed, so NOTHING was loaded. "No plugins are
    /// loaded" alone would have been true and useless.
    #[test]
    fn a_failure_shows_even_when_nothing_loaded() {
        let out = render_status_with_failures(&[], &[failure("init", "would not instantiate")]);
        assert!(out.contains("No plugins are loaded"), "{out}");
        assert!(out.contains("Failed to load (1)"), "{out}");
        assert!(out.contains("would not instantiate"), "{out}");
    }

    #[test]
    fn an_unknown_source_renders_a_dash_not_a_blank() {
        // A blank cell reads as a rendering bug; an em dash reads as "we do
        // not know", which is the truth for a hand-installed plugin.
        let out = render_status(&[sourced(
            "hand-installed",
            SourceRecord::Unknown,
            BuildState::NotBuilt,
        )]);
        let row = out
            .lines()
            .find(|l| l.contains("hand-installed"))
            .expect("row present");
        assert!(row.contains('—'), "got: {row:?}");
    }

    #[test]
    fn the_in_flight_states_render_distinctly() {
        // `building…` and `build-failed` are what a user sees after pressing
        // `b`; if either rendered as `—` the chord would look inert.
        let out = render_status(&[
            sourced(
                "busy",
                SourceRecord::Local("/x".into()),
                BuildState::Building,
            ),
            sourced(
                "broke",
                SourceRecord::Local("/y".into()),
                BuildState::Failed,
            ),
        ]);
        assert!(out.contains("building…"), "got: {out}");
        assert!(out.contains("build-failed"), "got: {out}");
    }

    #[test]
    fn capabilities_stay_last_so_the_table_stays_aligned() {
        // CAPABILITIES is the only variable-length cell (it trails a
        // `(denied: …)` note), so a column added after it would not line up.
        let out = render_status(&[sourced(
            "p",
            SourceRecord::Local("/x".into()),
            BuildState::Cached,
        )]);
        let header = out.lines().nth(2).expect("column header");
        let src_at = header.find("SOURCE").expect("SOURCE present");
        let build_at = header.find("BUILD").expect("BUILD present");
        let caps_at = header.find("CAPABILITIES").expect("CAPABILITIES present");
        assert!(src_at < build_at && build_at < caps_at, "got: {header:?}");
    }

    #[test]
    fn empty_state_is_explicit() {
        let out = render_status(&[]);
        assert!(out.contains("# Plugins (0 loaded)"));
        assert!(out.contains("No plugins are loaded"));
    }

    #[test]
    fn renders_a_row_per_plugin_with_health_and_tier() {
        let plugins = vec![
            status("fuzzy-finder", TrustTier::Bundled, PluginHealth::Healthy),
            status(
                "git-blame",
                TrustTier::UserInstalled,
                PluginHealth::Quarantined {
                    func: "on-event".into(),
                    kind: "fuel".into(),
                },
            ),
        ];
        let out = render_status(&plugins);
        assert!(out.contains("# Plugins (2 loaded)"));
        // Healthy row.
        assert!(out.contains("fuzzy-finder"));
        assert!(out.contains("bundled"));
        // Quarantined row shows the state AND the trap provenance suffix.
        let crashed_line = out
            .lines()
            .find(|l| l.contains("git-blame"))
            .expect("git-blame row present");
        assert!(crashed_line.contains("quarantined"));
        assert!(crashed_line.contains("user-installed"));
        assert!(crashed_line.contains("[trap: fuel in on-event]"));
    }

    #[test]
    fn capabilities_show_granted_and_denied() {
        let mut s = status(
            "cap-plugin",
            TrustTier::UserInstalled,
            PluginHealth::Healthy,
        );
        s.granted = vec![Capability::NetHttp("crates.io".into())];
        s.denied = vec![Capability::ProcSpawn];
        let out = render_status(&[s]);
        let row = out.lines().find(|l| l.contains("cap-plugin")).unwrap();
        assert!(
            row.contains("net:http:crates.io"),
            "granted cap in wire form"
        );
        assert!(row.contains("(denied: proc:spawn)"), "denied cap noted");
    }

    #[test]
    fn header_occupies_exactly_three_lines() {
        // The interactivity layer relies on the first plugin row landing at
        // line index `HEADER_LINES`. Pin it against a render drift.
        let out = render_status(&[status("first", TrustTier::Bundled, PluginHealth::Healthy)]);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("# Plugins"), "line 0 is the title");
        assert!(lines[1].is_empty(), "line 1 is blank");
        assert!(lines[2].contains("NAME"), "line 2 is the column header");
        assert!(
            lines[HEADER_LINES].contains("first"),
            "the first plugin row lands at line HEADER_LINES"
        );
    }

    #[test]
    fn empty_grant_renders_a_dash_not_blank() {
        let out = render_status(&[status("no-caps", TrustTier::Bundled, PluginHealth::Healthy)]);
        let row = out.lines().find(|l| l.contains("no-caps")).unwrap();
        assert!(row.trim_end().ends_with('—'), "empty caps render as a dash");
    }
}
