//! The `dashboard.*` theme element vocabulary (DB.3).
//!
//! `dashboard-mode` OWNS these elements — it registers them (and their
//! defaults) against the [`ThemeRegistry`] in `on_activate`
//! ([[feedback_mode_owns_its_surface]]). Registration is idempotent by name,
//! so re-activation is safe and always resolves to the same interned ids.
//! Themes override any of them through the normal element-override scopes; the
//! brand colours ship as `Literal` defaults so the mark reads correctly out of
//! the box under any theme, and the text roles reference palette keys so they
//! reskin naturally.
//!
//! The ids are consumed by the branding virtual-row provider (DB.4), which
//! resolves them into cell colours.

use lattice_theme::{Color, ColorRef, ElementId, ElementName, ElementOwner, StyleSpec, ThemeRegistry};

pub const ELEM_LOGO: &str = "dashboard.logo";
pub const ELEM_CURSOR: &str = "dashboard.cursor";
pub const ELEM_TITLE: &str = "dashboard.title";
pub const ELEM_TAGLINE: &str = "dashboard.tagline";
pub const ELEM_SECTION: &str = "dashboard.section";
pub const ELEM_KEY: &str = "dashboard.key";
pub const ELEM_LINK: &str = "dashboard.link";
pub const ELEM_BODY: &str = "dashboard.body";

/// Brand blue (`assets/lattice-mark.svg`).
pub const BRAND_BLUE: Color = Color::Rgb(0x1f, 0x6f, 0xeb);
/// Brand amber — the cursor bar inside the mark.
pub const BRAND_AMBER: Color = Color::Rgb(0xf5, 0x9e, 0x0b);

/// Interned ids for the `dashboard.*` elements, held in a `Copy` struct so
/// the branding provider (DB.4) reads `resolved.get(id)` by field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DashboardElementIds {
    pub logo: ElementId,
    pub cursor: ElementId,
    pub title: ElementId,
    pub tagline: ElementId,
    pub section: ElementId,
    pub key: ElementId,
    pub link: ElementId,
    pub body: ElementId,
}

/// Register the `dashboard.*` elements + defaults under `owner`. Idempotent by
/// name; returns the interned ids.
pub fn register_dashboard_theme_elements(
    reg: &dyn ThemeRegistry,
    owner: ElementOwner,
) -> DashboardElementIds {
    let elem = |name: &str, default: StyleSpec, doc: &'static str| {
        reg.register(ElementName::from(name.to_string()), owner.clone(), default, doc)
    };

    DashboardElementIds {
        // Brand colours ship as literals so the mark is correct under any
        // theme; still overridable.
        logo: elem(
            ELEM_LOGO,
            StyleSpec::new().fg(BRAND_BLUE),
            "Dashboard brand mark (the interlocking L bracket).",
        ),
        cursor: elem(
            ELEM_CURSOR,
            StyleSpec::new().fg(BRAND_AMBER),
            "Dashboard mark cursor bar.",
        ),
        title: elem(
            ELEM_TITLE,
            StyleSpec::new().fg(BRAND_BLUE),
            "Dashboard \"Lattice\" wordmark.",
        ),
        // Text roles reference palette keys so they reskin with the theme.
        tagline: elem(
            ELEM_TAGLINE,
            StyleSpec::new().fg(ColorRef::Palette("subtext".into())),
            "Dashboard tagline / muted hint text.",
        ),
        section: elem(
            ELEM_SECTION,
            StyleSpec::new().fg(ColorRef::Palette("blue".into())),
            "Dashboard section heading.",
        ),
        key: elem(
            ELEM_KEY,
            StyleSpec::new().fg(ColorRef::Palette("green".into())),
            "Dashboard key cap (e.g. `:`, `<leader>`).",
        ),
        link: elem(
            ELEM_LINK,
            StyleSpec::new().fg(ColorRef::Palette("blue".into())),
            "Dashboard followable link.",
        ),
        // Body inherits the default foreground.
        body: elem(ELEM_BODY, StyleSpec::new(), "Dashboard body text."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lattice_theme::{default_palette, InMemoryThemeRegistry};

    fn mode_owner() -> ElementOwner {
        ElementOwner::Mode("dashboard-mode".into())
    }

    #[test]
    fn registers_all_eight_under_mode_owner() {
        let reg = InMemoryThemeRegistry::new(default_palette());
        register_dashboard_theme_elements(&reg, mode_owner());
        for name in [
            ELEM_LOGO, ELEM_CURSOR, ELEM_TITLE, ELEM_TAGLINE, ELEM_SECTION, ELEM_KEY, ELEM_LINK,
            ELEM_BODY,
        ] {
            let info = reg
                .describe(&ElementName::from(name.to_string()))
                .unwrap_or_else(|| panic!("{name} should be registered"));
            assert_eq!(info.owner, mode_owner(), "{name} owner");
        }
    }

    #[test]
    fn brand_colours_are_literal_defaults() {
        let reg = InMemoryThemeRegistry::new(default_palette());
        register_dashboard_theme_elements(&reg, mode_owner());
        for name in [ELEM_LOGO, ELEM_TITLE] {
            let info = reg.describe(&ElementName::from(name.to_string())).unwrap();
            assert_eq!(info.default.fg, Some(ColorRef::Literal(BRAND_BLUE)));
        }
        let cursor = reg.describe(&ElementName::from(ELEM_CURSOR.to_string())).unwrap();
        assert_eq!(cursor.default.fg, Some(ColorRef::Literal(BRAND_AMBER)));
    }

    #[test]
    fn text_roles_reference_palette_keys() {
        let reg = InMemoryThemeRegistry::new(default_palette());
        register_dashboard_theme_elements(&reg, mode_owner());
        let tagline = reg.describe(&ElementName::from(ELEM_TAGLINE.to_string())).unwrap();
        assert_eq!(tagline.default.fg, Some(ColorRef::Palette("subtext".into())));
    }

    #[test]
    fn registration_is_idempotent() {
        let reg = InMemoryThemeRegistry::new(default_palette());
        let a = register_dashboard_theme_elements(&reg, mode_owner());
        let b = register_dashboard_theme_elements(&reg, mode_owner());
        assert_eq!(a, b, "re-registering must return the same interned ids");
    }
}
