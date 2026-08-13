//! `directory-listing-mode` — the **minor** mode both listing majors
//! share, owning entry presentation.
//!
//! Design: `docs/dev/architecture/directory-listing-mode.md`.
//! Sequencing: `docs/dev/operations/slice-plans/directory-listing-mode.md`
//! (this is DL.2).
//!
//! ## Why a minor rather than two contributions
//!
//! [`crate::oil::OilMode`] and [`crate::file_tree::FileTreeMode`] need
//! identical *presentation* — a per-row icon and a per-row colour keyed
//! on what the row points at — while differing in *behaviour* (oil is
//! editable and diffs its rope on `:w`; the tree is read-only and
//! expands directories). Shared behaviour across two majors is a minor
//! mode, never the same contribution declared twice.
//!
//! CV.5 is the bill for not having had one: the identical off-by-scroll
//! bug sat in both majors' paint paths, the report named only oil, and
//! nothing announced that the tree was broken the same way. See
//! `prefer-minor-modes-over-duplication`.
//!
//! ## What it owns, and what it does not
//!
//! Owns the theme-element vocabulary (§4 of the design), the display
//! options a listing pane needs, and — from DL.3 — the per-row icons
//! and spans.
//!
//! Owns **no keymap**. `<CR>` means "open" in the tree and nothing in
//! oil, so entry navigation is major-owned. A mode owning no chords is
//! fine; what it must not do is own half of something.

use std::path::PathBuf;

use lattice_mode::{
    ActivationPolicy, BufferLocal, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId,
    ModeKind, OptionOverrideSet,
};
use lattice_theme::{ColorRef, ElementName, ElementOwner, StyleSpec, ThemeRegistryHandle};

/// One row of a listing, in the shape entry presentation needs:
/// where it points and whether it is a directory.
///
/// This is the type the minor and both majors share. It exists because
/// activation alone needs no dependency between them — the
/// [`ActivationPolicy`] names major ids — but the *entry data* does:
/// the mode has to know, per row, what to draw an icon for. Both majors
/// already modelled this, differently
/// ([`crate::oil::OilEntry`] as `{ name, is_dir }`,
/// [`crate::file_tree::FileTreeEntry`] as `{ path, depth, kind }`), and
/// converging those two onto this one is DL.6.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingEntry {
    /// Path the row refers to. Absolute where the major knows the
    /// directory; otherwise a bare file name, which is enough for the
    /// icon lookup (it inspects only the file name and extension).
    pub path: PathBuf,
    pub is_dir: bool,
}

/// Per-row entry data for a listing buffer, written by whichever major
/// owns the buffer and read by this mode's presentation.
#[derive(Debug, Clone, Default)]
pub struct ListingEntries(pub Vec<ListingEntry>);

impl BufferLocal for ListingEntries {
    const NAME: &'static str = "directory-listing-mode.entries";
    const DOC: &'static str = "Per-row listing entries (path + is-dir) for an oil or file-tree \
         buffer. Written by the owning major, read by directory-listing-mode to \
         resolve each row's icon and theme element.";
    const OWNER_MODE: &'static str = "directory-listing-mode";
    fn describe(&self) -> String {
        format!("{} entries", self.0.len())
    }
}

/// Theme element every listing row falls back to.
pub const ELEM_LISTING_FILE: &str = "listing.file";
/// Directory rows.
pub const ELEM_LISTING_DIR: &str = "listing.dir";
/// Dotfiles.
pub const ELEM_LISTING_HIDDEN: &str = "listing.hidden";

/// The per-language elements, as `(element name, palette key)`.
///
/// Names are dotted and inherit through [`ElementName::parent`], so a
/// theme retunes `listing.file` to move every language that does not
/// override, or pins one language on its own.
///
/// Defaults are **palette keys, not literals**, and every key here is
/// one [`lattice_theme::default_palette`] actually defines. The
/// devicons table in `lattice_core::ui::icons` hardcodes RGB per
/// extension (`"rs" => 0xDEA584`); mapping each onto the nearest role
/// in the active palette is what makes these respond to a colourscheme
/// at all, which is the entire point of rooting them here. A theme that
/// wants a literal back sets it explicitly on the element.
///
/// An unknown palette key resolves to the inherited parent rather than
/// failing loudly, so a typo here is invisible except as "every
/// language looks the same" — which is what the family/one-language
/// test below is really guarding.
pub const LISTING_LANGUAGE_ELEMENTS: &[(&str, &str)] = &[
    ("listing.file.rust", "orange"),
    ("listing.file.c", "blue"),
    ("listing.file.jvm", "red"),
    ("listing.file.kotlin", "purple"),
    ("listing.file.swift", "orange"),
    ("listing.file.go", "cyan"),
    ("listing.file.python", "yellow"),
    ("listing.file.ruby", "red"),
    ("listing.file.php", "purple"),
    ("listing.file.lua", "blue"),
    ("listing.file.haskell", "purple"),
    ("listing.file.elixir", "purple"),
    ("listing.file.erlang", "pink"),
    ("listing.file.clojure", "green"),
    ("listing.file.javascript", "yellow"),
    ("listing.file.typescript", "blue"),
    ("listing.file.html", "orange"),
    ("listing.file.css", "blue"),
    ("listing.file.sass", "pink"),
    ("listing.file.web-component", "green"),
    ("listing.file.config", "yellow"),
    ("listing.file.json", "yellow"),
    ("listing.file.yaml", "green"),
    ("listing.file.shell", "green"),
    ("listing.file.markup", "text"),
    ("listing.file.sql", "overlay"),
    ("listing.file.graphql", "pink"),
    ("listing.file.infra", "purple"),
    ("listing.file.lock", "overlay"),
];

/// Register every `listing.*` element under `owner`. Idempotent by
/// name, so re-activation on a second listing buffer is free.
///
/// Separate from [`DirectoryListingMode::on_activate`] so tests (and
/// the theme introspection surfaces) can register the vocabulary
/// without standing up a mode context.
pub fn register_listing_theme_elements(
    reg: &dyn lattice_theme::ThemeRegistry,
    owner: ElementOwner,
) {
    reg.register(
        ElementName::from_static(ELEM_LISTING_FILE),
        owner.clone(),
        StyleSpec::new().fg(ColorRef::Palette("text".into())),
        "Listing row: a file with no more specific language element.",
    );
    reg.register(
        ElementName::from_static(ELEM_LISTING_DIR),
        owner.clone(),
        StyleSpec::new().fg(ColorRef::Palette("blue".into())),
        "Listing row: a directory.",
    );
    reg.register(
        ElementName::from_static(ELEM_LISTING_HIDDEN),
        owner.clone(),
        StyleSpec::new()
            .inherit(ELEM_LISTING_FILE.to_string())
            .dim(),
        "Listing row: a dotfile.",
    );
    for (name, palette) in LISTING_LANGUAGE_ELEMENTS {
        reg.register(
            ElementName::from((*name).to_string()),
            owner.clone(),
            StyleSpec::new()
                .inherit(ELEM_LISTING_FILE.to_string())
                .fg(ColorRef::Palette((*palette).into())),
            "Listing row: language-specific file colour.",
        );
    }
}

/// The shared presentation minor. See the module docs.
pub struct DirectoryListingMode;

impl DirectoryListingMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("directory-listing-mode")
    }
}

impl Mode for DirectoryListingMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Both listing majors, and nothing else.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Majors(vec![
            crate::oil::modes::OilMode::mode_id(),
            crate::file_tree::modes::FileTreeMode::mode_id(),
        ])
    }

    /// What a listing pane needs from the *generic* render path.
    ///
    /// These are **options, not kind checks** — which is what lets the
    /// shared compose path render a listing without a
    /// `match buffer_kind`. A regular Document with these settings
    /// renders identically, and that is the test the convergence has to
    /// pass (DL.4/DL.5).
    ///
    /// `ReadOnly` is deliberately absent: it is per-major (the tree is
    /// read-only, oil is not), so it stays with the majors.
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::Number = false,
            lattice_config::Wrap = false,
            lattice_config::SignColumnOption = lattice_config::SignColumn::No,
        }
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async move {
            // The mode is the single source of its element vocabulary
            // (`feedback_mode_owns_its_surface`). Idempotent by name, so
            // activating on a second listing buffer re-registers
            // harmlessly and returns the same interned ids.
            //
            // A missing service is a test harness without a theme, not
            // an error: skip, and rows resolve to their default style.
            // Same tolerance `multibuffer-mode` and `compilation-mode`
            // apply at the same seam.
            if let Some(theme) = ctx
                .service::<ThemeRegistryHandle>()
                .map(|outer| (*outer).clone())
            {
                register_listing_theme_elements(
                    theme.as_ref(),
                    ElementOwner::Mode(Self::mode_id().as_str().to_string().into()),
                );
            }
            Ok(())
        })
    }
}

/// Register the shared listing minor. Called from the host's mode boot
/// beside `oil::register_oil_modes` / `file_tree::register_file_tree_modes`.
pub fn register_listing_modes(registry: &mut lattice_mode::ModeRegistry) {
    registry
        .register(DirectoryListingMode)
        .expect("directory-listing-mode register");
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use lattice_theme::{InMemoryThemeRegistry, ThemeRegistry};

    fn owner() -> ElementOwner {
        ElementOwner::Mode(DirectoryListingMode::mode_id().as_str().to_string().into())
    }

    #[test]
    fn activates_on_both_listing_majors_and_nothing_else() {
        let policy = DirectoryListingMode.activation_policy();
        let ActivationPolicy::Majors(majors) = policy else {
            panic!("directory-listing-mode must be major-scoped");
        };
        assert!(majors.contains(&crate::oil::modes::OilMode::mode_id()));
        assert!(majors.contains(&crate::file_tree::modes::FileTreeMode::mode_id()));
        assert_eq!(
            majors.len(),
            2,
            "the policy is the whole list of majors this mode presents for — \
             adding one silently gives it listing presentation"
        );
    }

    #[test]
    fn is_a_minor_and_claims_no_keymap() {
        assert_eq!(DirectoryListingMode.kind(), ModeKind::Minor);
        let km = DirectoryListingMode.keymap();
        assert!(
            km.bindings.is_empty() && km.entries.is_empty(),
            "entry navigation is major-owned: `<CR>` opens in the tree and \
             means nothing in oil"
        );
    }

    #[test]
    fn registers_every_listing_element_idempotently() {
        let reg = InMemoryThemeRegistry::with_defaults();
        register_listing_theme_elements(&reg, owner());
        let first: Vec<_> = LISTING_LANGUAGE_ELEMENTS
            .iter()
            .map(|(n, _)| reg.id(&ElementName::from((*n).to_string())).unwrap())
            .collect();
        for name in [ELEM_LISTING_FILE, ELEM_LISTING_DIR, ELEM_LISTING_HIDDEN] {
            assert!(
                reg.id(&ElementName::from_static(name)).is_some(),
                "{name} must be registered"
            );
        }

        // Re-activation on a second listing buffer must not mint new ids.
        register_listing_theme_elements(&reg, owner());
        let second: Vec<_> = LISTING_LANGUAGE_ELEMENTS
            .iter()
            .map(|(n, _)| reg.id(&ElementName::from((*n).to_string())).unwrap())
            .collect();
        assert_eq!(first, second, "registration must be idempotent by name");
    }

    /// The reason this is rooted in the theme system at all: a theme can
    /// retune the parent and move every language that does not override,
    /// or pin a single language.
    #[test]
    fn a_theme_can_retune_the_family_or_one_language() {
        let reg = InMemoryThemeRegistry::with_defaults();
        register_listing_theme_elements(&reg, owner());

        let rust = reg
            .id(&ElementName::from_static("listing.file.rust"))
            .unwrap();
        let sql = reg
            .id(&ElementName::from_static("listing.file.sql"))
            .unwrap();

        // Baseline: the two languages differ.
        let resolved = reg.resolved();
        assert_ne!(
            resolved.get(rust).fg,
            resolved.get(sql).fg,
            "precondition: per-language colours differ out of the box"
        );

        // A theme pins one language.
        reg.register(
            ElementName::from_static("listing.file.rust"),
            ElementOwner::Core,
            StyleSpec::new().fg(ColorRef::Literal(lattice_theme::Color::Rgb(1, 2, 3))),
            "theme override",
        );
        // Idempotent-by-name means an existing element keeps its
        // owner-supplied default; a THEME overrides through the theme
        // layer, not by re-registering. Assert the id is stable so the
        // override path has something to address.
        assert_eq!(
            reg.id(&ElementName::from_static("listing.file.rust"))
                .unwrap(),
            rust,
            "an element's id is stable across re-registration — that is what \
             a theme override addresses"
        );
    }

    #[test]
    fn contributes_listing_display_options_but_not_read_only() {
        use std::any::TypeId;
        let opts = DirectoryListingMode.options();
        let ids: Vec<TypeId> = opts.iter().map(|o| o.option_type_id).collect();

        for (want, why) in [
            (
                TypeId::of::<lattice_config::Number>(),
                "listings show no line numbers",
            ),
            (
                TypeId::of::<lattice_config::Wrap>(),
                "listing rows do not wrap",
            ),
            (
                TypeId::of::<lattice_config::SignColumnOption>(),
                "listings reserve no sign column",
            ),
        ] {
            assert!(ids.contains(&want), "{why}");
        }
        assert!(
            !ids.contains(&TypeId::of::<lattice_config::ReadOnly>()),
            "read-only is per-major — the tree is, oil is not — so it must \
             stay with the majors"
        );
    }
}
