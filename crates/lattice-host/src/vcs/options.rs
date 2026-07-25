lattice_config::options! {
    group = lattice_config::Display;

    /// When `true`, files opened in a git repository automatically
    /// register a diff session against HEAD, showing gutter signs
    /// for added/removed/modified lines.
    ///
    /// Default `true`. Set to `false` to suppress auto-gutter-diff
    /// (the manual `:diff` / `:diffoff` commands still work).
    #[name("git.auto-head-diff")]
    pub GitAutoHeadDiff: bool = true;
}
