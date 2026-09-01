//! OM.A1 — the `WasmScannedExcerptSource` adapter.
//!
//! Wraps an agenda plugin's [`AgendaClient`] bridge and exposes a **native**-
//! typed producer the host's scan calls, exactly like `WasmMediaSource`. The
//! provider that drives it lives in `lattice-multibuffer` and knows nothing
//! about WASM; this is the only place the two meet.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lattice_mode::scanned_excerpt_source::{
    AgendaBeginFuture, AgendaFuture, AgendaRootsFuture, ScannedExcerpt, ScannedExcerptSource,
};

use crate::PluginId;
use crate::agenda_cache::AgendaCache;
use crate::agenda_task::{AgendaClient, Entry};

/// An async agenda-row producer over a plugin's [`AgendaClient`].
#[derive(Clone, Debug)]
pub struct WasmScannedExcerptSource {
    client: AgendaClient,
    /// Resolved ONCE at load, so the walk's per-file test is a string compare
    /// rather than a guest call. A `scan`-per-file boundary crossing is
    /// already the producer's dominant cost; adding an `extensions()` call
    /// per file would double it to answer a question that cannot change.
    extensions: Vec<String>,
    /// Resolved once at load, like `extensions` and for the same reason: the
    /// provider reads it on every open, and the answer cannot change.
    view_mode: Option<String>,
    /// OT.3b: scan results, persisted across restarts.
    ///
    /// It lives HERE, above `client`, because that is the layer where a hit can
    /// skip the guest call as well as the parse — a cache below the boundary
    /// could only ever have saved the parse. `Mutex` because `ScannedExcerptSource`
    /// hands out `&self` and the source is cloned into the scan task; `Arc` so
    /// every clone shares one cache rather than one each, which would make the
    /// hit rate depend on how many times the source was cloned.
    ///
    /// `None` when the host could not resolve a data directory. A cache that
    /// cannot be stored is simply not used.
    cache: Option<Arc<Mutex<AgendaCache>>>,
}

impl ScannedExcerptSource for WasmScannedExcerptSource {
    fn source_id(&self) -> u64 {
        self.plugin_id().0 as u64
    }

    fn extensions(&self) -> &[String] {
        &self.extensions
    }

    fn view_mode(&self) -> Option<&str> {
        self.view_mode.as_deref()
    }

    /// AF.1. NOT cached beside `extensions` / `view_mode`, and that asymmetry
    /// is the point: those are facts about the source, this is a fact about the
    /// user's config. Caching it would make the source's own setting appear not
    /// to work until the editor restarted.
    ///
    /// A guest that traps or is quarantined answers empty rather than failing
    /// the scan — the same degradation `begin` gets, one step earlier.
    fn roots(&self) -> AgendaRootsFuture<'_> {
        Box::pin(async move {
            match self.client.roots().await {
                Ok(roots) => Ok(roots),
                Err(e) => {
                    tracing::debug!(error = %e, "agenda: a source could not name its roots");
                    Ok(Vec::new())
                }
            }
        })
    }

    fn begin(&self) -> AgendaBeginFuture<'_> {
        Box::pin(async move {
            // OT.3b: `begin` now answers with the guest's generation key, and
            // handing it to the cache is what discards rows computed under an
            // anchor that no longer holds (org: yesterday's day, an old keyword
            // set). The host never learns what the key means.
            let generation = self
                .client
                .begin()
                .await
                .map_err(|e| format!("agenda plugin: {e}"))?;
            if let Some(cache) = &self.cache
                && let Ok(mut cache) = cache.lock()
            {
                cache.begin(generation);
            }
            Ok(())
        })
    }

    fn scan(&self, path: PathBuf, text: String) -> AgendaFuture<'_> {
        Box::pin(async move {
            let display = path.display().to_string();
            // OT.3b: an unchanged file skips the parse AND the guest call. The
            // read already happened upstream — you cannot know a file is
            // unchanged without looking at it — and a warm read is ~10-50 us
            // against the ~2 ms parse this avoids.
            if let Some(cache) = &self.cache
                && let Ok(mut cache) = cache.lock()
                && let Some(rows) = cache.get(&display, &text)
            {
                return Ok(rows
                    .into_iter()
                    .filter_map(|e| validate(&display, e))
                    .collect());
            }
            let raw: Vec<Entry> = match self.client.scan(display.clone(), text.clone()).await {
                // The guest's own `err` and a host-surface failure land in the
                // same place because the caller does the same thing with both:
                // skip this file, keep scanning.
                Ok(inner) => inner?,
                Err(host_err) => return Err(format!("agenda plugin: {host_err}")),
            };
            if let Some(cache) = &self.cache
                && let Ok(mut cache) = cache.lock()
            {
                cache.put(&display, &text, &raw);
            }
            Ok(raw
                .into_iter()
                .filter_map(|e| validate(&display, e))
                .collect())
        })
    }
}

impl WasmScannedExcerptSource {
    pub fn new(client: AgendaClient, extensions: Vec<String>, view_mode: Option<String>) -> Self {
        Self {
            client,
            extensions,
            view_mode,
            cache: None,
        }
    }

    /// OT.3b: give this source a persistent result cache rooted at `dir`
    /// (the plugin's own data directory, so two plugins cannot collide and
    /// uninstalling one removes its cache).
    ///
    /// Opt-in rather than built in `new`: a caller with no data directory —
    /// tests, benches — gets an uncached source that behaves identically, just
    /// slower, which is also what makes the cache easy to A/B.
    pub fn with_cache(mut self, dir: &std::path::Path) -> Self {
        let id = self.plugin_id().0 as u64;
        self.cache = Some(Arc::new(Mutex::new(AgendaCache::open(dir, id))));
        self
    }

    pub fn plugin_id(&self) -> PluginId {
        self.client.id()
    }
}

/// Normalise a declared extension: lowercase, leading dots stripped, blanks
/// dropped. A guest writing `".ORG"` and a guest writing `"org"` mean the same
/// filetype, and making the host guess which spelling it got is how a
/// producer ends up silently scanning nothing.
pub fn normalise_extensions(raw: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in raw {
        let cleaned = e.trim().trim_start_matches('.').to_ascii_lowercase();
        if cleaned.is_empty() {
            continue;
        }
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    out
}

/// Convert one WIT entry into a native one, or drop it.
///
/// Guest output is untrusted, exactly as `error_parser_host::validate` treats
/// a parsed diagnostic. The rejections here are the ones a buggy guest
/// actually produces: an inverted span (`end_line < line`, an off-by-one in
/// the guest's own subtree walk) and a line near `u32::MAX` (an underflow in a
/// 1-based → 0-based conversion). Both would otherwise become an excerpt that
/// renders nothing and jumps nowhere.
///
/// `debug!`, never `info!` — this fires per row of a project-wide scan.
fn validate(path: &str, e: Entry) -> Option<ScannedExcerpt> {
    if e.line == u32::MAX || e.end_line == u32::MAX {
        tracing::debug!(
            path,
            line = e.line,
            end_line = e.end_line,
            "agenda source returned an out-of-range line; skipping the row"
        );
        return None;
    }
    if e.end_line < e.line {
        tracing::debug!(
            path,
            line = e.line,
            end_line = e.end_line,
            "agenda source returned an inverted span; skipping the row"
        );
        return None;
    }
    Some(ScannedExcerpt {
        line: e.line,
        end_line: e.end_line,
        group: e.group,
        label: e.label,
        sort_key: e.sort_key,
        // OA.5: spans are guest output too, so they are validated rather than
        // trusted. Dropped PER SPAN, not per row — `display-span`'s own
        // contract is that one bad run must not cost a row its other runs,
        // and a row that vanished because its colour was wrong would be a
        // much worse failure than a row that renders plain.
        spans: e
            .spans
            .into_iter()
            .filter(|s| {
                let ok = s.end > s.start && !s.slot.is_empty();
                if !ok {
                    tracing::debug!(
                        path,
                        line = e.line,
                        start = s.start,
                        end = s.end,
                        slot = %s.slot,
                        "agenda source returned an empty or inverted span; skipping it"
                    );
                }
                ok
            })
            .map(|s| lattice_mode::scanned_excerpt_source::RowSpan {
                start: s.start,
                end: s.end,
                slot: s.slot,
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agenda_task::DisplaySpan;

    fn entry(line: u32, end_line: u32) -> Entry {
        Entry {
            line,
            end_line,
            group: "Today".into(),
            label: "TODO write tests".into(),
            sort_key: 42,
            spans: Vec::new(),
        }
    }

    /// OA.5: guest output is untrusted, and a bad span must cost the row its
    /// COLOUR, never the row. Dropped per span rather than per row —
    /// `display-span`'s own contract is that one bad run must not take the
    /// others with it, and a row that vanished because its colour was wrong
    /// would be a far worse failure than one that renders plain.
    #[test]
    fn a_bad_span_is_dropped_without_losing_the_row() {
        let mut e = entry(3, 3);
        e.spans = vec![
            // Inverted.
            DisplaySpan {
                start: 6,
                end: 2,
                slot: "keyword".into(),
            },
            // Empty.
            DisplaySpan {
                start: 4,
                end: 4,
                slot: "keyword".into(),
            },
            // No slot to resolve.
            DisplaySpan {
                start: 1,
                end: 3,
                slot: String::new(),
            },
            // The good one.
            DisplaySpan {
                start: 2,
                end: 6,
                slot: "keyword".into(),
            },
        ];
        let row = validate("/p/a.org", e).expect("the row survives its bad spans");
        assert_eq!(
            row.spans
                .iter()
                .map(|s| (s.start, s.end))
                .collect::<Vec<_>>(),
            vec![(2, 6)]
        );
        assert_eq!(row.spans[0].slot, "keyword");
    }

    #[test]
    fn a_well_formed_entry_converts() {
        let got = validate("/p/n.org", entry(9, 10)).expect("accepted");
        assert_eq!((got.line, got.end_line), (9, 10));
        assert_eq!(got.group, "Today");
        assert_eq!(got.sort_key, 42);
    }

    /// A single-line row is the common case and must not look inverted.
    #[test]
    fn a_single_line_span_is_accepted() {
        assert!(validate("/p/n.org", entry(4, 4)).is_some());
    }

    #[test]
    fn an_inverted_span_is_dropped() {
        assert!(validate("/p/n.org", entry(10, 9)).is_none());
    }

    /// What a guest's own 1-based → 0-based conversion produces when it
    /// underflows on line 0.
    #[test]
    fn an_out_of_range_line_is_dropped() {
        assert!(validate("/p/n.org", entry(u32::MAX, u32::MAX)).is_none());
        assert!(validate("/p/n.org", entry(0, u32::MAX)).is_none());
    }

    #[test]
    fn extensions_are_lowercased_and_dot_stripped() {
        let got = normalise_extensions(vec![".ORG".into(), "Md".into()]);
        assert_eq!(got, vec!["org".to_string(), "md".to_string()]);
    }

    /// A duplicate would make the walk offer one file to one source twice.
    #[test]
    fn duplicate_and_blank_extensions_are_dropped() {
        let got = normalise_extensions(vec![
            "org".into(),
            ".org".into(),
            "  ".into(),
            ".".into(),
            "".into(),
        ]);
        assert_eq!(got, vec!["org".to_string()]);
    }
}
