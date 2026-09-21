//! The `*plugins*` view's information headerline — two sticky virtual rows
//! above the buffer: what the plugin set looks like, and what you can do to it.
//!
//! **Why virtual rows rather than buffer text.** The interactivity layer maps
//! `cursor.line - render::HEADER_LINES` into the loaded-plugin list, and
//! `render.rs` warns that an extra header row "would silently put every chord
//! on the wrong plugin". Virtual rows paint outside the buffer's text, so this
//! header cannot shift that mapping — the invariant holds by construction
//! rather than by remembering.
//!
//! **Why a second provider.** `BuildHeaderline` (PM.8b) already occupies a
//! headerline row, but it is transient: it reports builds in flight and hides
//! itself when idle. Statistics and key hints are the opposite — always true,
//! never urgent. Fusing them would make a static row's version bump every time
//! a build started, and would couple two concerns with different lifetimes.
//!
//! **Why `VirtualRowProvider` directly** rather than the `Headerline`
//! convenience trait: that trait renders `Option<HeaderlineRow>` — one row.
//! This header is two. `collect()` returns them in insertion order, which
//! `AnchorPosition::Above` documents as paint order, so "stats above keys" is
//! deterministic rather than incidental.
//!
//! **Colour** comes from `role_fg` fallbacks today. The roles exist so a
//! theme-live pass can swap the lookup without touching the row builders —
//! `lattice-magit`'s `headerline.rs` is the pattern to follow when it does.

use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use lattice_cells::{
    AnchorPosition, Cell, ProviderId, VirtualRow, VirtualRowKind, VirtualRowProvider,
};
use lattice_plugin_host::TrustTier;
use lattice_plugin_loader::{FailedLoad, PluginHealth, PluginStatus};

/// Provider id for the information row pair. Distinct from
/// `BUILD_HEADERLINE_PROVIDER_ID` so both can register against the same
/// buffer — `VirtualRowRegistrar::register` rejects only a duplicate id.
pub const INFO_HEADERLINE_PROVIDER_ID: ProviderId = 0x706c_7567_6869_0800; // "plug-hi"

/// A semantic role for a run of cells. The fallback palette keeps the roles
/// visually distinct; a theme pass replaces `role_fg` and nothing else.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Role {
    /// A count that is merely informational.
    Count,
    /// A count that means something is wrong (quarantined, failed to load).
    Problem,
    /// A count that means everything is fine.
    Healthy,
    /// The literal keys a user presses.
    Key,
    /// Prose around the keys and counts.
    Label,
}

/// Fallback foregrounds, `0xRRGGBB`. Every role must be distinguishable from
/// every other — pinned by `every_role_is_visually_distinct`, the same
/// property `lattice-magit` asserts for its blame heading.
pub fn role_fg(role: Role) -> u32 {
    match role {
        Role::Count => 0xCDD6F4,
        Role::Problem => 0xF38BA8,
        Role::Healthy => 0xA6E3A1,
        Role::Key => 0x89B4FA,
        Role::Label => 0x6C7086,
    }
}

/// What the stats row reports. Derived once when the view renders, then
/// pushed into the provider — `version()` is polled on every cells tick and
/// must not do work, which is why this is cached rather than recomputed
/// (`PluginStatus` collection allocates; `builds_in_flight` is the only
/// lock-free counter the loader exposes).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginCounts {
    /// Plugins the host has loaded, healthy or not.
    pub loaded: usize,
    /// Loaded and running.
    pub ok: usize,
    /// Loaded but quarantined — still listed, no longer running. Matches
    /// `render::health_label`'s wording so the header and the table agree.
    pub quarantined: usize,
    /// Never loaded at all. These do not appear in the table's rows, so the
    /// header is the only place their existence is visible.
    pub failed: usize,
    /// Shipped with the editor rather than installed by the user.
    pub bundled: usize,
}

/// Count a status snapshot. `failed` is a separate list because a plugin that
/// failed to load has no `PluginStatus` — it is not in the table at all.
pub fn counts(plugins: &[PluginStatus], failed: &[FailedLoad]) -> PluginCounts {
    let mut c = PluginCounts {
        loaded: plugins.len(),
        failed: failed.len(),
        ..PluginCounts::default()
    };
    for p in plugins {
        match p.health {
            PluginHealth::Healthy => c.ok += 1,
            PluginHealth::Quarantined { .. } => c.quarantined += 1,
        }
        if p.tier == TrustTier::Bundled {
            c.bundled += 1;
        }
    }
    c
}

/// A run of text sharing one role — the unit the row builders emit.
struct Run(String, Role);

fn cells_from(runs: &[Run]) -> Vec<Cell> {
    let mut cells = Vec::new();
    for Run(text, role) in runs {
        let fg = role_fg(*role);
        cells.extend(text.chars().map(|ch| Cell::new(ch as u32, fg, 0, 0)));
    }
    cells
}

/// `12 plugins · 11 ok · 1 quarantined · 2 failed to load · 3 bundled`
///
/// Zero-valued problem counts are omitted: a view with nothing wrong should
/// not display `0 crashed`, which reads as a category to worry about.
fn stats_runs(c: &PluginCounts) -> Vec<Run> {
    let mut runs = vec![
        Run(format!("{} ", c.loaded), Role::Count),
        Run(
            if c.loaded == 1 { "plugin" } else { "plugins" }.to_string(),
            Role::Label,
        ),
    ];
    let mut push = |n: usize, label: &str, role: Role| {
        if n > 0 {
            runs.push(Run(" · ".to_string(), Role::Label));
            runs.push(Run(format!("{n} "), role));
            runs.push(Run(label.to_string(), Role::Label));
        }
    };
    push(c.ok, "ok", Role::Healthy);
    push(c.quarantined, "quarantined", Role::Problem);
    push(c.failed, "failed to load", Role::Problem);
    push(c.bundled, "bundled", Role::Count);
    runs
}

/// `r/R reload  b/B rebuild  u/U update  x unload  X clean  K describe  t trace`
///
/// Grouped by the mode's own convention — lowercase acts on the plugin under
/// the cursor, uppercase on every plugin — because the convention is more
/// useful to learn than twelve individual chords. Every chord the keymap binds
/// must appear here; `the_hint_row_names_every_chord_the_keymap_binds` fails
/// when one is added without a hint, since a gap in a hand-kept list does not
/// announce itself.
fn keys_runs() -> Vec<Run> {
    let pairs: &[(&str, &str)] = &[
        ("r/R", "reload"),
        ("b/B", "rebuild"),
        ("u/U", "update"),
        ("x/X", "unload/clean"),
        ("K", "describe"),
        ("t/T", "trace"),
    ];
    let mut runs = Vec::new();
    for (i, (keys, what)) in pairs.iter().enumerate() {
        if i > 0 {
            runs.push(Run("  ".to_string(), Role::Label));
        }
        runs.push(Run((*keys).to_string(), Role::Key));
        runs.push(Run(format!(" {what}"), Role::Label));
    }
    runs.push(Run("   (UPPER = all)".to_string(), Role::Label));
    runs
}

/// The provider. Rows are rebuilt from cached counts, so `collect()` costs
/// one small allocation and `version()` costs an atomic load.
#[derive(Debug)]
pub struct InfoHeaderline {
    counts: RwLock<PluginCounts>,
    version: AtomicU64,
}

impl InfoHeaderline {
    pub fn new(counts: PluginCounts) -> Self {
        Self {
            counts: RwLock::new(counts),
            version: AtomicU64::new(1),
        }
    }

    /// Push fresh counts. Bumps the version only when they actually changed,
    /// so an unchanged re-render does not invalidate the cells cache.
    pub fn set(&self, next: PluginCounts) {
        let changed = match self.counts.write() {
            Ok(mut slot) => {
                let changed = *slot != next;
                *slot = next;
                changed
            }
            // A poisoned lock means a writer panicked. Report no change rather
            // than panicking on the cells worker's path; the row goes stale,
            // which is strictly better than taking the view down.
            Err(_) => false,
        };
        if changed {
            self.version.fetch_add(1, Ordering::Release);
        }
    }

    fn snapshot(&self) -> PluginCounts {
        self.counts
            .read()
            .map(|c| *c)
            .unwrap_or_else(|_| PluginCounts::default())
    }
}

fn sticky_row(cells: Vec<Cell>) -> VirtualRow {
    VirtualRow {
        media: None,
        anchor_line: 0,
        position: AnchorPosition::Above,
        cells: cells.into(),
        height: 1,
        kind: VirtualRowKind::Sticky,
        bg: None,
        scales: None,
        gutter_line: None,
        gutter_fg: None,
    }
}

impl VirtualRowProvider for InfoHeaderline {
    fn id(&self) -> ProviderId {
        INFO_HEADERLINE_PROVIDER_ID
    }

    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    fn collect(&self) -> Vec<VirtualRow> {
        let c = self.snapshot();
        // Insertion order is paint order for `Above` rows at the same anchor,
        // so stats sits above keys deterministically.
        vec![
            sticky_row(cells_from(&stats_runs(&c))),
            sticky_row(cells_from(&keys_runs())),
        ]
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn text_of(row: &VirtualRow) -> String {
        row.cells
            .iter()
            .filter(|c| c.codepoint != 0)
            .map(|c| char::from_u32(c.codepoint).unwrap_or('?'))
            .collect()
    }

    #[test]
    fn every_role_is_visually_distinct() {
        // A role that resolves to another role's colour is a role that does
        // not exist as far as the reader is concerned.
        let roles = [
            Role::Count,
            Role::Problem,
            Role::Healthy,
            Role::Key,
            Role::Label,
        ];
        for (i, a) in roles.iter().enumerate() {
            for b in &roles[i + 1..] {
                assert_ne!(
                    role_fg(*a),
                    role_fg(*b),
                    "{a:?} and {b:?} resolve to the same colour"
                );
            }
        }
    }

    #[test]
    fn the_header_is_two_rows_stats_above_keys() {
        let h = InfoHeaderline::new(PluginCounts {
            loaded: 3,
            ok: 3,
            bundled: 3,
            ..PluginCounts::default()
        });
        let rows = h.collect();
        assert_eq!(rows.len(), 2, "stats row and keys row");
        assert!(
            text_of(&rows[0]).contains("plugins"),
            "first row is the stats row: {}",
            text_of(&rows[0])
        );
        assert!(
            text_of(&rows[1]).contains("reload"),
            "second row is the keys row: {}",
            text_of(&rows[1])
        );
        for r in &rows {
            assert_eq!(r.anchor_line, 0);
            assert_eq!(r.position, AnchorPosition::Above);
            assert_eq!(r.kind, VirtualRowKind::Sticky);
            assert_eq!(r.height, 1);
        }
    }

    #[test]
    fn zero_valued_problem_counts_are_omitted() {
        // `0 crashed` reads as a category to worry about. A healthy view
        // should not mention crashes at all.
        let h = InfoHeaderline::new(PluginCounts {
            loaded: 2,
            ok: 2,
            bundled: 2,
            ..PluginCounts::default()
        });
        let stats = text_of(&h.collect()[0]);
        assert!(!stats.contains("quarantined"), "{stats}");
        assert!(!stats.contains("failed"), "{stats}");
        assert!(stats.contains("2 ok"), "{stats}");
    }

    #[test]
    fn a_quarantined_or_failed_plugin_is_reported() {
        let h = InfoHeaderline::new(PluginCounts {
            loaded: 4,
            ok: 3,
            quarantined: 1,
            failed: 2,
            bundled: 1,
        });
        let stats = text_of(&h.collect()[0]);
        assert!(stats.contains("1 quarantined"), "{stats}");
        assert!(stats.contains("2 failed to load"), "{stats}");
    }

    #[test]
    fn the_loaded_count_is_singular_for_one_plugin() {
        let h = InfoHeaderline::new(PluginCounts {
            loaded: 1,
            ok: 1,
            ..PluginCounts::default()
        });
        assert!(text_of(&h.collect()[0]).starts_with("1 plugin "));
    }

    #[test]
    fn version_bumps_only_when_the_counts_change() {
        // `version()` is polled on every cells tick; an unchanged push must
        // not invalidate the cache.
        let h = InfoHeaderline::new(PluginCounts::default());
        let v0 = h.version();
        h.set(PluginCounts::default());
        assert_eq!(h.version(), v0, "an identical push is not a change");
        h.set(PluginCounts {
            loaded: 1,
            ok: 1,
            ..PluginCounts::default()
        });
        assert!(h.version() > v0, "a real change bumps the version");
    }

    #[test]
    fn counts_classify_a_mixed_snapshot() {
        // Built from real `PluginStatus` values rather than hand-set counts,
        // so the classification itself is under test.
        use lattice_plugin_loader::{BuildState, SourceRecord};
        let mk = |name: &str, tier: TrustTier, health: PluginHealth| PluginStatus {
            id: 0,
            name: name.to_string(),
            tier,
            granted: Vec::new(),
            denied: Vec::new(),
            health,
            source: SourceRecord::Unknown,
            build: BuildState::NotBuilt,
        };
        let plugins = vec![
            mk("auto-pair", TrustTier::Bundled, PluginHealth::Healthy),
            mk("project", TrustTier::Bundled, PluginHealth::Healthy),
            mk(
                "third-party",
                TrustTier::UserInstalled,
                PluginHealth::Healthy,
            ),
        ];
        let c = counts(&plugins, &[]);
        assert_eq!(c.loaded, 3);
        assert_eq!(c.ok, 3);
        assert_eq!(c.quarantined, 0);
        assert_eq!(c.bundled, 2, "only the bundled tier counts as bundled");
        assert_eq!(c.failed, 0);
    }

    #[test]
    fn the_hint_row_names_every_chord_the_keymap_binds() {
        // The hint list is kept by hand, and a missing entry is invisible —
        // the chord still works, it just stops being discoverable. This fails
        // when a chord is added to the keymap without a hint.
        let hint = text_of(&InfoHeaderline::new(PluginCounts::default()).collect()[1]);
        for entry in crate::mode::plugins_keymap_entries_for_test() {
            let chord = entry.chord;
            // `<CR>` is an alias for `K` (describe) and needs no separate hint.
            if chord == "<CR>" {
                continue;
            }
            assert!(
                hint.contains(chord),
                "chord `{chord}` ({}) is bound but absent from the hint row: {hint}",
                entry.doc
            );
        }
    }
}
