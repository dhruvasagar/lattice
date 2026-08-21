//! PL8.H.2 — render a `Vec<PluginStatus>` (the loader's read model) into the
//! `*plugins*` buffer's text. Pure + presentation-only: the loader hands over
//! structured `PluginStatus` (typed capabilities + health), and the view owns
//! how it looks. No I/O, no allocation on any hot path (this runs off-thread on
//! mode activation / a crash re-render).

use lattice_plugin_host::{Capability, TrustTier};
use lattice_plugin_loader::{PluginHealth, PluginStatus};

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
    let mut out = format!("# Plugins ({} loaded)\n\n", plugins.len());
    if plugins.is_empty() {
        out.push_str("No plugins are loaded. Load one with `:plugin-load <path>`.\n");
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
