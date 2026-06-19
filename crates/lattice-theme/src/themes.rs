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
use crate::palette::{catppuccin_latte_palette, default_palette, macchiato_palette, Palette};

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
        // T.11.1: first LIGHT theme. The palette indirection + the
        // canvas elements (T.11.0b) mean a swap to Latte recolours the
        // whole surface light with no per-element overrides.
        NamedTheme {
            name: "catppuccin-latte",
            palette: catppuccin_latte_palette(),
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
        let mauve = crate::PaletteKey::from_static("purple");
        assert_ne!(mocha.palette.get(&mauve), mac.palette.get(&mauve));
        assert_eq!(
            mac.palette.get(&mauve),
            Some(Color::Rgb(0xc6, 0xa0, 0xf6))
        );
    }

    #[test]
    fn latte_is_a_registered_light_theme() {
        // T.11.1: catppuccin-latte is registered and is genuinely LIGHT —
        // its `base` (the canvas background, via `editor.background`) is a
        // light colour, inverted from Mocha's dark base, and its `text`
        // (the foreground, via `editor.foreground`) is dark. This is what
        // makes the T.11.0b palette-driven canvas render light on a swap.
        let themes = builtin_themes();
        let latte = themes
            .iter()
            .find(|t| t.name == "catppuccin-latte")
            .expect("catppuccin-latte registered");
        let base = crate::PaletteKey::from_static("base");
        let text = crate::PaletteKey::from_static("text");
        assert_eq!(latte.palette.get(&base), Some(Color::Rgb(0xef, 0xf1, 0xf5)));
        assert_eq!(latte.palette.get(&text), Some(Color::Rgb(0x4c, 0x4f, 0x69)));
        // Light base: every channel brighter than Mocha's dark base.
        let mocha = themes.iter().find(|t| t.name == "catppuccin-mocha").unwrap();
        let (Some(Color::Rgb(lr, lg, lb)), Some(Color::Rgb(mr, mg, mb))) =
            (latte.palette.get(&base), mocha.palette.get(&base))
        else {
            panic!("both bases are rgb");
        };
        assert!(lr > mr && lg > mg && lb > mb, "Latte base must be lighter than Mocha");
    }
}
