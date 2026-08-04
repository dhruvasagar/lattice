// `linkme`'s distributed slices use `link_section` to aggregate
// items at link time. The `options!` macro expansion in this file
// emits such a declaration; allow the workspace's `unsafe_code =
// "deny"` lint locally with the same safety rationale documented in
// `option_decl.rs`, `group.rs`, and `core_options.rs`.
#![allow(unsafe_code)]

//! Per-pane behaviour options (`pane.*`).
//!
//! PBH.4: the bound on each pane's buffer-history trail, walked with
//! `<C-6>` / `<C-7>`. See
//! `docs/dev/architecture/pane-buffer-history.md` §9.

crate::options! {
    group = crate::Pane;

    /// How many buffers one pane remembers in its back/forward trail
    /// (`<C-6>` / `<C-7>`, `:history pane-buffers`).
    ///
    /// Oldest entries are evicted past this bound. Each entry is a
    /// buffer id plus a cursor position, so the default is cheap; raise
    /// it if you routinely walk further back than a hundred buffer
    /// switches within one pane.
    ///
    /// Values below 1 are clamped to 1 — a zero-length trail would have
    /// nowhere to store the buffer the pane is currently showing.
    #[name("pane.buffer-history-size")]
    pub PaneBufferHistorySize: i64 = 100;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use crate::{ConfigRegistry, PaneBufferHistorySize};

    fn reg() -> ConfigRegistry {
        let r = ConfigRegistry::new();
        r.init_from_linkme();
        r
    }

    #[test]
    fn default_is_100() {
        let r = reg();
        assert_eq!(*r.get_typed::<PaneBufferHistorySize>().unwrap(), 100);
    }

    #[test]
    fn is_settable() {
        let r = reg();
        r.parse_and_set_command("pane.buffer-history-size=25")
            .unwrap();
        assert_eq!(*r.get_typed::<PaneBufferHistorySize>().unwrap(), 25);
    }

    #[test]
    fn the_option_is_registered_under_its_dotted_name() {
        // `:set pane.buffer-history-size` / `:customize` reach it by
        // name; a typo in `#[name(..)]` would only show up here.
        let r = reg();
        assert!(r.lookup("pane.buffer-history-size").is_some());
    }
}
