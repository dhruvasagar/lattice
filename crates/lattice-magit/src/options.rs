//! MG.22b: the options `magit-hunk-mode` owns — the first options
//! `lattice-magit` registers at all.
//!
//! Until this slice the crate registered none, which several user-doc
//! pages had to say out loud: every `magit.*` name a user reached for
//! failed with `unknown option`.
//!
//! **`magit.hunk.syntax-highlight` is deliberately absent**, though the
//! design fragment lists it. It would gate language-aware hunk content,
//! and that feature does not exist — an option that changes nothing is
//! the same failure as a menu row that does nothing, just quieter,
//! because `:set` reports success. It lands with the feature.
//!
//! **`magit.hunk.line-backgrounds` is absent too, and lives elsewhere
//! instead**: it became `ui.diff.line-backgrounds` in `lattice-diff`.
//! MG.21a found the mechanism is generic —
//! `Editor::diff_signs_from_spans` derives the tint from whatever spans
//! a mode publishes, so every diff-showing buffer shares it — and an
//! option named for one consumer would have understated what it turns
//! off.

/// Validator for `magit.hunk.context-lines`. `0` is meaningful (show
/// only the changed lines); the ceiling stops a fat-fingered value
/// turning every diff into the whole file.
#[allow(clippy::ptr_arg)]
fn validate_context_lines(n: &i64) -> Result<(), String> {
    if *n >= 0 && *n <= 1000 {
        Ok(())
    } else {
        Err(format!(
            "magit.hunk.context-lines must be in range [0, 1000], got {n}"
        ))
    }
}

lattice_config::options! {
    group = lattice_config::Magit;

    /// Unchanged lines of context around each hunk in the diffs magit
    /// generates — `git diff -U<n>`. Default `3`, which is git's own.
    ///
    /// **Distinct from `ui.diff.context`**, which is how much context a
    /// *fold* leaves visible inside a two-pane diff session. This one
    /// decides how much context git puts in the patch text to begin
    /// with; that one decides how much of an existing diff stays
    /// unfolded.
    ///
    /// `D` in a diff buffer overrides it for that view (MG.23k). The
    /// override wins: this is the default for views that have not been
    /// told otherwise, not a floor.
    #[name("magit.hunk.context-lines")]
    #[validate(validate_context_lines)]
    pub MagitHunkContextLines: i64 = 3;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_context_is_allowed_and_a_silly_value_is_not() {
        // `0` is meaningful — show only the changed lines.
        assert!(validate_context_lines(&0).is_ok());
        assert!(validate_context_lines(&3).is_ok());
        assert!(validate_context_lines(&1000).is_ok());
        assert!(validate_context_lines(&-1).is_err());
        assert!(validate_context_lines(&1001).is_err());
    }

    /// The message has to name the option, since `:set` reports it
    /// verbatim and "out of range" alone says nothing about which
    /// setting was refused.
    #[test]
    fn the_rejection_names_the_option_and_the_value() {
        let e = validate_context_lines(&-5).expect_err("negative is refused");
        assert!(e.contains("magit.hunk.context-lines"), "{e}");
        assert!(e.contains("-5"), "{e}");
    }
}
