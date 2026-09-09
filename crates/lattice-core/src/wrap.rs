//! `autowrap` — whether typing past `textwidth` breaks the line.
//!
//! Lives here rather than in `lattice-config` for the same reason
//! [`crate::IndentMethod`] does: the reflow engine in `lattice-grammar`
//! consumes the resolved value, and `lattice-config` → `lattice-grammar`
//! would be the wrong direction. This crate is the shared floor both
//! sides already stand on.
//!
//! See `docs/dev/architecture/text-reflow.md` §8.

/// `textwidth`, resolved for a specific buffer — the column reflow
/// targets and, when `autowrap` allows, the column typing breaks at.
///
/// **A newtype rather than a bare `usize`, and that is load-bearing.**
/// It is carried by two env structs (`GrammarEnv` and the actor's
/// `DispatchEnv`), both of which derive `Default` for their ~40
/// hand-built call sites. A bare `usize` defaults to `0`, which is not a
/// column anything can wrap at — so every one of those sites would
/// silently get a reflow that does nothing, and the bug would look like
/// "gq is broken in tests but works in the editor". Giving the *type* a
/// meaningful default puts the answer in one place and makes a third env
/// struct impossible to get wrong.
///
/// Same reasoning that made [`crate::IndentUnit`] a type rather than
/// three loose fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct WrapWidth(pub usize);

/// The column a buffer wraps at when nobody has resolved config.
///
/// Matches `lattice_config::core_options::TextWidth`'s registered
/// default; `lattice-core` cannot import it (the dependency points the
/// other way), so a test in `lattice-config` — which can see both —
/// pins them together.
pub const DEFAULT_TEXTWIDTH: usize = 80;

impl Default for WrapWidth {
    fn default() -> Self {
        WrapWidth(DEFAULT_TEXTWIDTH)
    }
}

impl WrapWidth {
    /// The target column.
    pub fn columns(self) -> usize {
        self.0
    }
}

impl From<usize> for WrapWidth {
    fn from(n: usize) -> Self {
        WrapWidth(n)
    }
}

crate::labeled_enum! {
    /// `:set autowrap=...`. Whether inserting a character past
    /// `textwidth` breaks the line and carries the remainder down.
    ///
    /// ## Why this exists as one named option
    ///
    /// Vim has no toggle for this at all — it is `formatoptions`, a bag
    /// of nineteen single-letter flags of which `t` (wrap text) and `c`
    /// (wrap comments) are the two anyone means. That is why nobody
    /// remembers them. Emacs has `auto-fill-mode`, which names a 1980s
    /// implementation ("filling") rather than the effect.
    ///
    /// ## Why it is an option and not a minor mode
    ///
    /// It owns no keymap, no lifecycle subscription, no decoration
    /// provider and no buffer — it is a behaviour flag consulted on the
    /// insert path, which is exactly what `electricindent` (IN.6)
    /// already is. Same seam, same per-major override mechanism via
    /// `Mode::options()`. A mode that is really a bool is a mode in name
    /// only.
    ///
    /// ## Not to be confused with `wrap`
    ///
    /// `wrap` is SOFT wrap — a display decision, no bytes change. This
    /// inserts real newlines. The two are orthogonal and compose: a
    /// buffer may soft-wrap at the window edge while hard-wrapping at
    /// `textwidth`.
    pub enum AutoWrap {
        /// Never break a line while typing. `gq` still reflows on
        /// demand — `textwidth` remains the measure either way.
        Off = "off" | "false" | "no"
            => "Never wrap while typing (gq still reflows on demand)",
        /// Break only inside comments, judged by the line's leading
        /// comment marker. The default for code: a long comment wraps,
        /// a long string literal does not.
        #[default]
        Comments = "comments"
            => "Wrap comment lines only (the default for code)",
        /// Break any line past `textwidth`. The default for prose
        /// majors — markdown, org, text, git commit messages.
        All = "all" | "true" | "yes"
            => "Wrap any line past textwidth (the default for prose)",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn labels_round_trip() {
        for v in AutoWrap::all() {
            assert_eq!(AutoWrap::parse_label(v.label()).unwrap(), *v);
        }
    }

    /// `off`/`all` take boolean-ish aliases because the option reads as
    /// a toggle to anyone arriving from `auto-fill-mode` or from
    /// `formatoptions+=t`, and `:set autowrap=true` failing with
    /// "invalid value" would be a papercut for no gain.
    #[test]
    fn boolean_aliases_parse_to_the_ends_of_the_range() {
        for yes in ["all", "true", "yes"] {
            assert_eq!(AutoWrap::parse_label(yes).unwrap(), AutoWrap::All);
        }
        for no in ["off", "false", "no"] {
            assert_eq!(AutoWrap::parse_label(no).unwrap(), AutoWrap::Off);
        }
    }

    /// The default is `comments`, not `all`. Auto-wrapping a code line
    /// mid-expression is destructive in a way wrapping prose is not, so
    /// the global default is the conservative rung and prose majors opt
    /// up through `Mode::options()` (RF.4).
    #[test]
    fn the_global_default_is_comments() {
        assert_eq!(AutoWrap::default(), AutoWrap::Comments);
    }

    #[test]
    fn an_unknown_label_is_rejected() {
        assert!(AutoWrap::parse_label("sometimes").is_err());
    }
}
