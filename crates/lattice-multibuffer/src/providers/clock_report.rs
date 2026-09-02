//! OA.16 — the clock report: clocked time, rolled up an outline.
//!
//! Design: `docs/dev/architecture/org-agenda.md` §3. Slice plan:
//! [`org-agenda.md`](../../../../docs/dev/operations/slice-plans/org-agenda.md).
//!
//! Emacs calls this a clocktable, and `org-agenda-clockreport-mode` puts one at
//! the top of the agenda. It answers "where did the week go", which is a
//! different question from "what should I do next" — and that is why it is a
//! REPORT over the scan's clock spans rather than a view of the agenda's rows.
//!
//! ## Totals roll UP, and that is the whole shape
//!
//! A span is filed against the headline it was clocked on. A report that listed
//! only those would answer "which leaf did I clock" and never "how long did the
//! project take", which is the question anyone opens a clocktable for. So every
//! span contributes to each of its ancestors as well as to itself.
//!
//! [`ClockSpan::outline`] carries the whole chain precisely because of this: an
//! ancestor that logged no time of its own emits no span at all, so the chain
//! is the only way to name it. Rebuilding the tree from leaf names plus levels
//! would invent a parent for every orphan and mis-nest two projects that happen
//! to share a child name.
//!
//! ## Own time versus total time
//!
//! Both are kept. `total` is what rolls up; `own` is what was clocked on that
//! headline itself. A tree showing only totals cannot distinguish a project
//! whose time is all in its children from one where half was spent on the
//! parent, and that difference is usually the interesting part of the report.
//!
//! ## The range is a display choice, not a scan
//!
//! Spans are held per view unfiltered (see `ScanViewState::clock`), so
//! switching the report between a day, a week and a year filters data already
//! in hand. Re-walking the corpus to answer a question the data contains would
//! make a toggle cost a scan.

use lattice_mode::ClockSpan;

/// One line of the report: a headline, its depth, and its two totals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReportRow {
    /// Outline depth, 0 for a top-level headline. Drives the indent.
    pub depth: usize,
    pub title: String,
    /// Minutes clocked on this headline and everything under it.
    pub total: u32,
    /// Minutes clocked on this headline itself.
    pub own: u32,
}

/// A whole report: its rows in outline order, and the grand total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClockReport {
    pub rows: Vec<ReportRow>,
    pub total: u32,
}

impl ClockReport {
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Build a report from `spans`, keeping only those filed in `days`.
///
/// `days` is an inclusive epoch-day range. An empty range or no matching span
/// yields an empty report, which the caller renders as "no clocked time" rather
/// than as an empty table — a table with no rows and a `0:00` total reads as a
/// broken report, where a sentence reads as an answer.
///
/// Rows come out in **outline order**: a parent immediately before its
/// children, siblings in first-seen order. Not sorted by time, deliberately —
/// the report is a picture of the outline, and re-ordering it by duration
/// breaks the one structural cue that says which rows belong to which project.
pub fn build(spans: &[ClockSpan], days: std::ops::RangeInclusive<i64>) -> ClockReport {
    // Insertion-ordered accumulation keyed by the outline PREFIX, so a path is
    // the identity of a node and two projects sharing a child name never merge.
    let mut order: Vec<Vec<String>> = Vec::new();
    let mut totals: std::collections::HashMap<Vec<String>, (u32, u32)> =
        std::collections::HashMap::new();

    for span in spans.iter().filter(|s| days.contains(&s.day)) {
        for depth in 0..span.outline.len() {
            let prefix: Vec<String> = span.outline[..=depth].to_vec();
            let entry = totals.entry(prefix.clone()).or_insert_with(|| {
                order.push(prefix.clone());
                (0, 0)
            });
            // Every ancestor takes the span's minutes as TOTAL; only the
            // headline it was clocked on takes them as OWN.
            entry.0 = entry.0.saturating_add(span.minutes);
            if depth + 1 == span.outline.len() {
                entry.1 = entry.1.saturating_add(span.minutes);
            }
        }
    }

    // Outline order: sort the collected paths lexicographically by their
    // first-seen index at each level, which is what `order` already records —
    // a stable sort on the path itself would alphabetise siblings and lose the
    // file's own order.
    let index: std::collections::HashMap<&Vec<String>, usize> =
        order.iter().enumerate().map(|(i, p)| (p, i)).collect();
    let mut paths = order.clone();
    paths.sort_by_key(|p| {
        // A node sorts by the first-seen index of each of its ancestors in
        // turn, so a child always follows its parent and siblings keep the
        // order the scan met them in.
        (0..p.len())
            .map(|d| index.get(&p[..=d].to_vec()).copied().unwrap_or(usize::MAX))
            .collect::<Vec<_>>()
    });

    let rows: Vec<ReportRow> = paths
        .iter()
        .filter_map(|p| {
            let (total, own) = totals.get(p)?;
            Some(ReportRow {
                depth: p.len() - 1,
                title: p.last().cloned().unwrap_or_default(),
                total: *total,
                own: *own,
            })
        })
        .collect();

    // The grand total is the sum of the TOP-LEVEL rows, not of every row:
    // summing all of them would count a child's minutes once for itself and
    // again in each ancestor.
    let total = rows
        .iter()
        .filter(|r| r.depth == 0)
        .fold(0u32, |acc, r| acc.saturating_add(r.total));

    ClockReport { rows, total }
}

/// `H:MM`, org's own clocktable spelling.
///
/// Not `1.5h`: org writes `1:30`, every clocktable a user has seen writes
/// `1:30`, and a report that agreed with the file's `=> 1:30` lines in every
/// place but its own summary would read as a rounding bug.
pub fn format_minutes(minutes: u32) -> String {
    format!("{}:{:02}", minutes / 60, minutes % 60)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    fn span(outline: &[&str], day: i64, minutes: u32) -> ClockSpan {
        ClockSpan {
            line: 0,
            outline: outline.iter().map(|s| s.to_string()).collect(),
            day,
            minutes,
        }
    }

    #[test]
    fn no_spans_is_an_empty_report() {
        assert!(build(&[], 0..=10).is_empty());
    }

    /// The range filters. A report titled "this week" that quietly included
    /// last week's time would be wrong in the direction nobody checks.
    #[test]
    fn only_spans_in_range_count() {
        let spans = [span(&["A"], 5, 60), span(&["A"], 99, 30)];
        let r = build(&spans, 0..=10);
        assert_eq!(r.total, 60);
        assert_eq!(r.rows.len(), 1);
    }

    /// The whole shape: time clocked on a child counts for every ancestor.
    /// Without this the report answers "which leaf did I clock" and never
    /// "how long did the project take".
    #[test]
    fn totals_roll_up_the_outline() {
        let spans = [span(&["Project", "Task"], 1, 90)];
        let r = build(&spans, 0..=10);
        assert_eq!(
            r.rows,
            vec![
                ReportRow {
                    depth: 0,
                    title: "Project".into(),
                    total: 90,
                    own: 0,
                },
                ReportRow {
                    depth: 1,
                    title: "Task".into(),
                    total: 90,
                    own: 90,
                },
            ]
        );
        assert_eq!(r.total, 90);
    }

    /// `own` is what distinguishes a project whose time is all in its children
    /// from one where half was spent on the parent — usually the interesting
    /// part of the report.
    #[test]
    fn own_time_is_kept_beside_the_total() {
        let spans = [span(&["Project"], 1, 30), span(&["Project", "Task"], 1, 60)];
        let r = build(&spans, 0..=10);
        assert_eq!(r.rows[0].total, 90, "the parent totals both");
        assert_eq!(r.rows[0].own, 30, "…and owns only its own");
        assert_eq!(r.total, 90, "the grand total counts the time ONCE");
    }

    /// An ancestor that logged no time of its own still appears — it emits no
    /// span, so the outline chain is the only thing that can name it.
    #[test]
    fn a_parent_that_clocked_nothing_is_still_named() {
        let spans = [span(&["Silent", "Loud"], 1, 15)];
        let r = build(&spans, 0..=10);
        assert_eq!(r.rows[0].title, "Silent");
        assert_eq!(r.rows[0].own, 0);
    }

    /// Two projects with a same-named child must not merge. The path is the
    /// identity; a name plus a level is not.
    #[test]
    fn same_named_children_under_different_parents_stay_apart() {
        let spans = [span(&["A", "Notes"], 1, 10), span(&["B", "Notes"], 1, 20)];
        let r = build(&spans, 0..=10);
        let notes: Vec<&ReportRow> = r.rows.iter().filter(|x| x.title == "Notes").collect();
        assert_eq!(notes.len(), 2, "two distinct rows: {:?}", r.rows);
        assert_eq!(r.total, 30);
    }

    /// A child follows its parent, and siblings keep the order the scan met
    /// them in — the report is a picture of the outline, and sorting by
    /// duration would break the cue that says which rows belong together.
    #[test]
    fn rows_come_out_in_outline_order() {
        let spans = [
            span(&["A", "Second"], 1, 5),
            span(&["A", "First"], 1, 500),
            span(&["B"], 1, 1),
        ];
        let r = build(&spans, 0..=10);
        let shape: Vec<(usize, &str)> =
            r.rows.iter().map(|x| (x.depth, x.title.as_str())).collect();
        assert_eq!(
            shape,
            vec![(0, "A"), (1, "Second"), (1, "First"), (0, "B")],
            "parent first, siblings in first-seen order — NOT by duration"
        );
    }

    /// Several spans on one headline across the range add up.
    #[test]
    fn repeated_spans_on_one_headline_sum() {
        let spans = [span(&["A"], 1, 20), span(&["A"], 2, 25)];
        assert_eq!(build(&spans, 0..=10).total, 45);
    }

    /// org's own spelling. A report that disagreed with the `=> 1:30` lines in
    /// the file would read as a rounding bug.
    #[test]
    fn minutes_format_the_way_org_writes_them() {
        assert_eq!(format_minutes(0), "0:00");
        assert_eq!(format_minutes(5), "0:05");
        assert_eq!(format_minutes(60), "1:00");
        assert_eq!(format_minutes(90), "1:30");
        assert_eq!(format_minutes(605), "10:05");
    }
}

// ── The virtual-row provider ────────────────────────────────────────────────

use lattice_cells::Cell;
use lattice_cells::virtual_rows::{AnchorPosition, VirtualRow, VirtualRowKind, VirtualRowProvider};
use lattice_core::BufferId;
use std::sync::{Arc, RwLock};

/// Element name: the clock report's own rows.
pub const ELEM_CLOCKREPORT: &str = "scan-view.clockreport";
/// Element name: the report's total line, which is the row people look at.
pub const ELEM_CLOCKREPORT_TOTAL: &str = "scan-view.clockreport.total";

/// The `ProviderId` a view's clock report registers under.
///
/// Derived from the view's `BufferId` exactly as the multibuffer's own two
/// providers are, so two open scan views each get their own report and
/// re-activating a mode replaces its rows rather than doubling them
/// (`register_virtual_row_provider` dedups by id — the OA.14 spike's finding).
pub fn clock_report_provider_id(view: BufferId) -> u64 {
    // A distinct salt from the excerpt-header / status providers so the three
    // never collide on one view.
    u64::from(view.0).wrapping_mul(4).wrapping_add(3)
}

/// Renders a [`ClockReport`] as virtual rows above line 0 of a scan view.
///
/// Display-only, and that is the design (org-agenda.md §3): a clock report has
/// no source range and nothing to jump to, which is exactly what `VirtualRow`
/// models. The cost — you cannot put the cursor on a report line — is stated in
/// the design rather than discovered here; making those lines actionable means
/// excerpts over a synthetic pathless source, which is deferred.
pub struct ClockReportProvider {
    view: BufferId,
    state: Arc<RwLock<crate::providers::scan_view::ScanViewState>>,
    /// The inclusive epoch-day range the report covers, recomputed by the mode
    /// when the view's span changes.
    days: RwLock<std::ops::RangeInclusive<i64>>,
    fg: u32,
    total_fg: u32,
    /// Bumped whenever the range changes, so the worker re-runs `collect`
    /// without waiting for the document's line count to move.
    version: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for ClockReportProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClockReportProvider")
            .field("view", &self.view)
            .finish()
    }
}

impl ClockReportProvider {
    pub fn new(
        view: BufferId,
        state: Arc<RwLock<crate::providers::scan_view::ScanViewState>>,
        days: std::ops::RangeInclusive<i64>,
        fg: u32,
        total_fg: u32,
    ) -> Self {
        Self {
            view,
            state,
            days: RwLock::new(days),
            fg,
            total_fg,
            version: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Re-point the report at a different range — what `gD`'s day/week/month
    /// toggles change. Bumps the version so the rows rebuild.
    pub fn set_days(&self, days: std::ops::RangeInclusive<i64>) {
        if let Ok(mut d) = self.days.write() {
            *d = days;
        }
        self.version
            .fetch_add(1, std::sync::atomic::Ordering::Release);
    }

    /// The report as lines of text, which is what both the renderer and the
    /// tests want — splitting this out keeps the row-building testable without
    /// a theme or a worker.
    pub fn lines(&self) -> Vec<String> {
        let Ok(state) = self.state.read() else {
            return Vec::new();
        };
        let days = self
            .days
            .read()
            .map(|d| d.clone())
            .unwrap_or_else(|_| 0..=0);
        let report = build(&state.clock, days);
        if report.is_empty() {
            // A sentence, not an empty table. A table with no rows and a `0:00`
            // total reads as a broken report; "no clocked time" reads as an
            // answer, and it is the answer more often than not.
            return vec!["  No clocked time in range".to_string()];
        }
        let mut out = Vec::with_capacity(report.rows.len() + 1);
        out.push(format!("  Clock total  {}", format_minutes(report.total)));
        for row in &report.rows {
            // Two columns: total, then own in parentheses when it differs —
            // showing `(0:00)` on every leaf-less parent would be noise, and
            // showing own == total on a leaf says nothing.
            let own = if row.own != row.total && row.own > 0 {
                format!("  ({})", format_minutes(row.own))
            } else {
                String::new()
            };
            out.push(format!(
                "  {:indent$}{}  {}{}",
                "",
                row.title,
                format_minutes(row.total),
                own,
                indent = row.depth * 2,
            ));
        }
        out
    }
}

impl VirtualRowProvider for ClockReportProvider {
    fn id(&self) -> u64 {
        clock_report_provider_id(self.view)
    }

    fn version(&self) -> u64 {
        // The scan's own generation folded in, so a completed re-scan rebuilds
        // the report — otherwise `gr` would leave last scan's totals on screen
        // under this scan's rows, which is the class of staleness a report can
        // least afford.
        let scan = self.state.read().map(|s| s.clock.len() as u64).unwrap_or(0);
        self.version
            .load(std::sync::atomic::Ordering::Acquire)
            .wrapping_add(scan)
    }

    fn collect(&self) -> Vec<VirtualRow> {
        self.lines()
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                let fg = if i == 0 { self.total_fg } else { self.fg };
                let cells: Vec<Cell> = text
                    .chars()
                    .map(|c| Cell::new(c as u32, fg, 0, 0))
                    .collect();
                VirtualRow {
                    anchor_line: 0,
                    // Above line 0, so the report sits at the top of the view
                    // the way emacs' clocktable does.
                    position: AnchorPosition::Above,
                    cells: Arc::from(cells),
                    height: 1,
                    // `Annotation`: content that scrolls with its anchor and
                    // paints no backdrop. `Generic` carries the diff
                    // deletion-block backdrop, which on a report would read as
                    // removed lines.
                    kind: VirtualRowKind::Annotation,
                    bg: None,
                    scales: None,
                    media: None,
                    gutter_line: None,
                    gutter_fg: None,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod provider_tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::providers::scan_view::ScanViewState;

    fn state_with(spans: Vec<ClockSpan>) -> Arc<RwLock<ScanViewState>> {
        Arc::new(RwLock::new(ScanViewState {
            provider: "agenda".to_string(),
            options: Default::default(),
            clock: spans,
        }))
    }

    fn span(outline: &[&str], day: i64, minutes: u32) -> ClockSpan {
        ClockSpan {
            line: 0,
            outline: outline.iter().map(|s| s.to_string()).collect(),
            day,
            minutes,
        }
    }

    fn provider(spans: Vec<ClockSpan>, days: std::ops::RangeInclusive<i64>) -> ClockReportProvider {
        ClockReportProvider::new(BufferId(7), state_with(spans), days, 0x999999, 0xffffff)
    }

    /// A sentence, not an empty table. `0:00` under a header reads as a broken
    /// report; this reads as an answer, and it is the answer more often than
    /// not.
    #[test]
    fn an_empty_range_says_so_rather_than_drawing_an_empty_table() {
        let p = provider(vec![span(&["A"], 99, 60)], 0..=10);
        assert_eq!(p.lines(), vec!["  No clocked time in range"]);
    }

    #[test]
    fn the_total_leads_and_the_tree_follows() {
        let p = provider(
            vec![span(&["Project"], 1, 30), span(&["Project", "Task"], 1, 60)],
            0..=10,
        );
        let lines = p.lines();
        assert_eq!(lines[0], "  Clock total  1:30");
        assert!(lines[1].starts_with("  Project  1:30"), "{lines:?}");
        assert!(
            lines[1].contains("(0:30)"),
            "own time is shown when it differs from the total: {lines:?}"
        );
        assert!(
            lines[2].starts_with("    Task  1:00"),
            "the child is indented under it: {lines:?}"
        );
        assert!(
            !lines[2].contains('('),
            "a leaf's own IS its total, so saying it twice is noise: {lines:?}"
        );
    }

    /// Re-pointing the range is what `gD`'s day/week toggles do, and the rows
    /// have to rebuild — a version that did not move would leave last range's
    /// totals on screen.
    #[test]
    fn changing_the_range_bumps_the_version_and_the_rows() {
        let p = provider(vec![span(&["A"], 5, 60)], 0..=1);
        let before = p.version();
        assert_eq!(p.lines(), vec!["  No clocked time in range"]);
        p.set_days(0..=10);
        assert!(p.version() > before, "the worker must be told to rebuild");
        assert_eq!(p.lines()[0], "  Clock total  1:00");
    }

    /// Rows sit ABOVE line 0 and paint no backdrop — `Generic` carries the
    /// diff deletion-block backdrop, which on a report would read as removed
    /// lines.
    #[test]
    fn rows_are_annotations_above_the_first_line() {
        let p = provider(vec![span(&["A"], 1, 60)], 0..=10);
        let rows = p.collect();
        assert!(!rows.is_empty());
        for row in &rows {
            assert_eq!(row.anchor_line, 0);
            assert_eq!(row.position, AnchorPosition::Above);
            assert_eq!(row.kind, VirtualRowKind::Annotation);
        }
    }

    /// Two views get two providers, so opening a second agenda cannot make one
    /// report replace the other's.
    #[test]
    fn each_view_gets_its_own_provider_id() {
        assert_ne!(
            clock_report_provider_id(BufferId(1)),
            clock_report_provider_id(BufferId(2))
        );
    }
}

// ── The mode ────────────────────────────────────────────────────────────────

/// The keymap target `cr` fires.
pub const TOGGLE_ACTION: &str = "action:scan-view-clockreport-toggle";

/// OA.16 — `scan-view-clockreport-mode`: the clock report, on a scan view.
///
/// Named generically rather than `org-agenda-clockreport-mode`, because
/// nothing here is org's. The data is [`ClockSpan`], a field of the generic
/// `ScanResult` any scanned-excerpt-source may report; a source that reports
/// clock spans gets a clock report, and one that does not gets an empty one.
/// Calling it `org-…` in the host would be the org-shaped constant MV.3
/// removed from this module once already.
///
/// **The toggle is the MODE, not the provider.** Activating registers the
/// provider, deactivating unregisters it, so there is one switch rather than
/// two states that can disagree — and `:describe-mode` answers "is the clock
/// report on" without anyone having to expose provider state.
#[derive(Debug, Default)]
pub struct ScanViewClockReportMode;

impl ScanViewClockReportMode {
    pub fn mode_id() -> lattice_mode::ModeId {
        lattice_mode::ModeId::new("scan-view-clockreport-mode")
    }
}

impl lattice_mode::Mode for ScanViewClockReportMode {
    type Guard = ();

    fn id(&self) -> lattice_mode::ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> lattice_mode::ModeKind {
        lattice_mode::ModeKind::Minor
    }

    /// Manual, for `ScanViewMode`'s reason: no policy can say "the view this
    /// provider just built", and one keyed on `BufferKind::Multibuffer` would
    /// attach a clock report to every search result and diff.
    fn activation_policy(&self) -> lattice_mode::ActivationPolicy {
        lattice_mode::ActivationPolicy::Manual
    }

    /// Registers the report's rows for the buffer it was activated on.
    ///
    /// `&self` throughout: `VirtualRowRegistrar` is a service with interior
    /// mutability precisely so a mode can do this from `on_activate`, where
    /// the `&mut` `ModeActivator` is not reachable.
    ///
    /// A view with no scan state yet registers nothing rather than an empty
    /// report — the mode can be activated before the first scan lands, and a
    /// report that said "no clocked time" while the scan was still running
    /// would be answering a question nobody had asked yet.
    fn on_activate(&self, ctx: lattice_mode::ModeContext) -> lattice_mode::LifecycleFuture<'_, ()> {
        Box::pin(async move {
            // `ModeContext` speaks the protocol's `BufferId`; every registry
            // here speaks core's. One conversion at the boundary rather than
            // one per call.
            let view = lattice_core::BufferId(ctx.buffer_id().0 as u32);
            let Some(svc) = ctx.service::<crate::providers::scan_view::ScanViewServiceHandle>()
            else {
                return Ok(());
            };
            let Some(state) = svc.state(view) else {
                return Ok(());
            };
            let Some(registrar) = ctx.service::<Arc<dyn lattice_mode::VirtualRowRegistrar>>()
            else {
                return Ok(());
            };
            // Today by default; `gD` (OA.18) widens it. See `today_range`.
            let days = today_range();
            let (fg, total_fg) = (0x9a9a9a, 0xd8d8d8);
            registrar.register(
                view,
                Arc::new(ClockReportProvider::new(view, state, days, fg, total_fg)),
            );
            Ok(())
        })
    }
}

/// The inclusive epoch-day range a fresh report covers: today.
///
/// **The report's range is its OWN, not the view's.** The host cannot read the
/// view's window even in principle — the span and offset live in `scan_args`,
/// which the host carries "as something it can route but not read" (see
/// `begin`) because they are the guest's vocabulary. That is not a gap here:
/// `ScanViewState::clock` is documented as holding every span UNFILTERED
/// precisely so the range stays a display choice, switched between day, week,
/// month and year by `gD` (OA.18) without re-walking the corpus.
///
/// Today, because a day is the range whose answer changes most and the one a
/// glance is usually asking about. `set_days` widens it.
fn today_range() -> std::ops::RangeInclusive<i64> {
    let today = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| (d.as_secs() / 86_400) as i64)
        // An unreadable clock yields day 0 rather than a guess: 1970 reports
        // nothing, which is conspicuously wrong, where a plausible wrong day
        // would report someone else's hours as today's.
        .unwrap_or(0);
    today..=today
}

#[cfg(test)]
mod mode_tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_mode::Mode;

    /// The mode is a MANUAL minor, for `ScanViewMode`'s reason: no policy can
    /// say "the view this provider just built", and one keyed on
    /// `BufferKind::Multibuffer` would put a clock report on every search
    /// result and every diff.
    #[test]
    fn it_is_a_manual_minor() {
        let m = ScanViewClockReportMode;
        assert_eq!(m.kind(), lattice_mode::ModeKind::Minor);
        assert!(matches!(
            m.activation_policy(),
            lattice_mode::ActivationPolicy::Manual
        ));
        assert_eq!(m.id().as_str(), "scan-view-clockreport-mode");
    }

    /// Generically named. Nothing in the report is org's — the data is
    /// `ClockSpan`, a field of the generic `ScanResult` that any
    /// scanned-excerpt-source may report — and an `org-` prefix in the host is
    /// the vocabulary MV.3 removed from this module once already.
    #[test]
    fn the_mode_is_not_named_after_org() {
        assert!(
            !ScanViewClockReportMode::mode_id().as_str().contains("org"),
            "the host does not name a generic mechanism after one plugin"
        );
    }
}
