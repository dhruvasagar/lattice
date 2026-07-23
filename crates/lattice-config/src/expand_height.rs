//! Value type for the `command-line.expand-height` typed option
//! (rich-minibuffer MB.2e).
//!
//! When the `:` command line is expanded into its full-modal
//! mini-buffer band (`<C-x><C-e>`), this option decides how tall
//! the band grows. It is pure display policy read by the renderers
//! (TUI + GPUI) — like [`crate::SignColumn`] — so the value type
//! lives in `lattice-config` and impls [`OptionType`] locally.
//!
//! Default is [`ExpandHeight::Half`] (the band claims half the
//! frame, the MB.2b/c behaviour). `full` grows it as tall as the
//! frame allows (leaving one pane row); a bare integer pins it to a
//! fixed row count. The renderer resolves the policy against the
//! *current* frame height via [`ExpandHeight::rows`] — the host
//! publishes the policy, the renderer applies it, because only the
//! renderer knows the live frame height.

use crate::option_type::{EnumeratedValue, OptionType};

/// `command-line.expand-height` — how tall the expanded `:` band grows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExpandHeight {
    /// Half the frame height (clamped to a sane minimum). The default,
    /// matching the MB.2b/c band.
    #[default]
    Half,
    /// As tall as the frame allows, leaving a single pane row above.
    Full,
    /// A fixed number of rows, clamped to what the frame can show.
    Fixed(u16),
}

impl ExpandHeight {
    /// Resolve the policy to a concrete band height for a frame of
    /// `frame_height` rows. Always leaves at least one row for the
    /// pane area above the band (`max`), and never returns 0. `Half`
    /// preserves the original MB.2b clamp (≥3 rows where the frame
    /// permits). Pure + total — no panic even for tiny frames (the
    /// lower bound is itself clamped to `max`).
    pub fn rows(&self, frame_height: u16) -> u16 {
        // Upper bound: everything except one pane row and (implicitly)
        // the tabline the layout reserves. Never below 1.
        let max = frame_height.saturating_sub(2).max(1);
        match self {
            ExpandHeight::Half => {
                let lo = 3.min(max);
                (frame_height / 2).clamp(lo, max)
            }
            ExpandHeight::Full => max,
            ExpandHeight::Fixed(n) => (*n).clamp(1, max),
        }
    }

    pub fn label(&self) -> String {
        match self {
            ExpandHeight::Half => "half".to_string(),
            ExpandHeight::Full => "full".to_string(),
            ExpandHeight::Fixed(n) => n.to_string(),
        }
    }

    pub fn doc(&self) -> &'static str {
        match self {
            ExpandHeight::Half => "Half the frame height (default)",
            ExpandHeight::Full => "As tall as the frame allows (one pane row kept)",
            ExpandHeight::Fixed(_) => "A fixed number of rows",
        }
    }

    pub fn parse_label(s: &str) -> Result<Self, String> {
        match s.trim() {
            "half" => Ok(ExpandHeight::Half),
            "full" => Ok(ExpandHeight::Full),
            other => other.parse::<u16>().map(ExpandHeight::Fixed).map_err(|_| {
                format!(
                    "command-line.expand-height: expected `half`, `full`, or a row count, got `{other}`"
                )
            }),
        }
    }
}

impl OptionType for ExpandHeight {
    fn parse(s: &str) -> Result<Self, String> {
        ExpandHeight::parse_label(s)
    }
    fn format(&self) -> String {
        self.label()
    }
    fn type_label() -> &'static str {
        "expand-height"
    }
    fn enumerate() -> Option<Vec<&'static str>> {
        // The `Fixed(n)` case is free-form, so only the two named
        // forms enumerate for `<Tab>` completion; a bare number is
        // still accepted by `parse`.
        Some(vec!["half", "full"])
    }
    fn enumerate_with_docs() -> Option<Vec<EnumeratedValue>> {
        Some(vec![
            EnumeratedValue {
                form: "half",
                doc: ExpandHeight::Half.doc(),
            },
            EnumeratedValue {
                form: "full",
                doc: ExpandHeight::Full.doc(),
            },
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_half() {
        assert_eq!(ExpandHeight::default(), ExpandHeight::Half);
    }

    #[test]
    fn parse_round_trips_named_and_numeric() {
        assert_eq!(ExpandHeight::parse_label("half").unwrap(), ExpandHeight::Half);
        assert_eq!(ExpandHeight::parse_label("full").unwrap(), ExpandHeight::Full);
        assert_eq!(
            ExpandHeight::parse_label("12").unwrap(),
            ExpandHeight::Fixed(12)
        );
        assert_eq!(ExpandHeight::Fixed(12).format(), "12");
        assert_eq!(ExpandHeight::Half.format(), "half");
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(ExpandHeight::parse_label("tall").is_err());
        assert!(ExpandHeight::parse_label("-3").is_err());
    }

    #[test]
    fn half_matches_the_original_mb2_clamp() {
        // 40-row frame → half is 20, within [3, 38].
        assert_eq!(ExpandHeight::Half.rows(40), 20);
        // Tiny frame stays sane (no panic, never 0).
        assert_eq!(ExpandHeight::Half.rows(4), 2);
        assert_eq!(ExpandHeight::Half.rows(2), 1);
    }

    #[test]
    fn full_leaves_one_pane_row() {
        assert_eq!(ExpandHeight::Full.rows(40), 38);
        assert_eq!(ExpandHeight::Full.rows(3), 1);
    }

    #[test]
    fn fixed_is_clamped_to_the_frame() {
        assert_eq!(ExpandHeight::Fixed(10).rows(40), 10);
        // Asking for more than the frame allows clamps to `max`.
        assert_eq!(ExpandHeight::Fixed(100).rows(40), 38);
        // Never 0.
        assert_eq!(ExpandHeight::Fixed(0).rows(40), 1);
    }
}
