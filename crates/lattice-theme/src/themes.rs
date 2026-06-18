//! Named builtin themes (T.9.b).
//!
//! A [`NamedTheme`] bundles a palette with an (optional) element-
//! override set under a user-typeable name. `:colorscheme <name>`
//! looks a theme up here and hands its `palette` + `overrides` to
//! [`ThemeRegistry::set_theme`](crate::ThemeRegistry::set_theme),
//! which swaps the active palette + override set atomically and marks
//! the resolved table dirty. The renderers rebuild on the host's
//! `ThemeChanged` signal.
//!
//! v1 ships two Catppuccin flavours. Both keep an empty override list
//! — the palette indirection already re-colours every element, so a
//! flavour swap needs no per-element overrides. A future theme that
//! wants to restyle a specific element (not just re-tint via the
//! palette) populates `overrides`.
//!
//! Design: `docs/dev/architecture/theme-system.md` §3.3, §5.1.

use crate::element::{ElementName, StyleSpec};
use crate::palette::{default_palette, macchiato_palette, Palette};

/// A named theme: a palette + a (possibly empty) element-override set.
/// The unit a `:colorscheme <name>` swap resolves to.
pub struct NamedTheme {
    pub name: &'static str,
    pub palette: Palette,
    pub overrides: Vec<(ElementName, StyleSpec)>,
}

/// Every builtin theme, by name. `:colorscheme` matches `name`
/// case-sensitively against this list. The first entry
/// (`catppuccin-mocha`) is the boot default — its palette equals
/// [`default_palette`], so swapping to it is a no-op restore.
pub fn builtin_themes() -> Vec<NamedTheme> {
    vec![
        NamedTheme {
            name: "catppuccin-mocha",
            palette: default_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "catppuccin-macchiato",
            palette: macchiato_palette(),
            overrides: Vec::new(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Color;

    #[test]
    fn builtin_themes_has_both_flavours() {
        let themes = builtin_themes();
        let names: Vec<&str> = themes.iter().map(|t| t.name).collect();
        assert!(names.contains(&"catppuccin-mocha"));
        assert!(names.contains(&"catppuccin-macchiato"));
    }

    #[test]
    fn macchiato_mauve_differs_from_mocha() {
        let themes = builtin_themes();
        let mocha = themes
            .iter()
            .find(|t| t.name == "catppuccin-mocha")
            .expect("mocha registered");
        let mac = themes
            .iter()
            .find(|t| t.name == "catppuccin-macchiato")
            .expect("macchiato registered");
        let mauve = crate::PaletteKey::from_static("mauve");
        assert_ne!(mocha.palette.get(&mauve), mac.palette.get(&mauve));
        assert_eq!(
            mac.palette.get(&mauve),
            Some(Color::Rgb(0xc6, 0xa0, 0xf6))
        );
    }
}
