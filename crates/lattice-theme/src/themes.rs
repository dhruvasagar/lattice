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
use crate::palette::{
    catppuccin_latte_palette, default_palette, dracula_dark_palette, dracula_light_palette,
    everforest_dark_palette, everforest_light_palette, gruvbox_dark_palette, gruvbox_light_palette,
    macchiato_palette, monokai_dark_palette, monokai_light_palette, nord_dark_palette,
    nord_light_palette, one_dark_palette, one_light_palette, rosepine_dark_palette,
    rosepine_light_palette, solarized_dark_palette, solarized_light_palette, tokyonight_dark_palette,
    tokyonight_light_palette, Palette,
};

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
        // T.11.1: the multi-theme library — 9 cross-editor families ×
        // {dark, light}. Each palette fills the full role-key set, so an
        // empty override list still recolours the whole surface.
        NamedTheme {
            name: "gruvbox-dark",
            palette: gruvbox_dark_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "gruvbox-light",
            palette: gruvbox_light_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "tokyonight-dark",
            palette: tokyonight_dark_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "tokyonight-light",
            palette: tokyonight_light_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "dracula-dark",
            palette: dracula_dark_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "dracula-light",
            palette: dracula_light_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "nord-dark",
            palette: nord_dark_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "nord-light",
            palette: nord_light_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "solarized-dark",
            palette: solarized_dark_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "solarized-light",
            palette: solarized_light_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "one-dark",
            palette: one_dark_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "one-light",
            palette: one_light_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "everforest-dark",
            palette: everforest_dark_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "everforest-light",
            palette: everforest_light_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "rosepine-dark",
            palette: rosepine_dark_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "rosepine-light",
            palette: rosepine_light_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "monokai-dark",
            palette: monokai_dark_palette(),
            overrides: Vec::new(),
        },
        NamedTheme {
            name: "monokai-light",
            palette: monokai_light_palette(),
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

    #[test]
    fn every_builtin_theme_covers_the_full_role_key_set() {
        // T.11.1: each builtin palette MUST define every key that
        // `default_palette` defines, or element resolution falls back to
        // the inherit chain / hard default — a silent mis-theme. This pin
        // guarantees no fallback for any registered theme.
        let reference = default_palette();
        for theme in builtin_themes() {
            for key in reference.keys() {
                assert!(
                    theme.palette.get(key).is_some(),
                    "theme `{}` is missing role key `{}`",
                    theme.name,
                    key.as_str()
                );
            }
        }
    }

    #[test]
    fn light_themes_are_light_and_dark_themes_are_dark() {
        // T.11.1: a `*-light` theme's `base` (canvas background) must be
        // brighter (sum of channels) than its `text` (foreground); a
        // `*-dark` theme's `base` must be darker than its `text`. Catches a
        // copy-paste that left a light palette with a dark base or vice
        // versa.
        fn channel_sum(c: Color) -> u32 {
            match c {
                Color::Rgb(r, g, b) => r as u32 + g as u32 + b as u32,
                other => {
                    let rgb = other.to_rgb_u32(0);
                    ((rgb >> 16) & 0xff) + ((rgb >> 8) & 0xff) + (rgb & 0xff)
                }
            }
        }
        let base = crate::PaletteKey::from_static("base");
        let text = crate::PaletteKey::from_static("text");
        for theme in builtin_themes() {
            let b = channel_sum(theme.palette.get(&base).expect("base defined"));
            let t = channel_sum(theme.palette.get(&text).expect("text defined"));
            if theme.name.ends_with("-light") || theme.name.ends_with("-latte") {
                assert!(
                    b > t,
                    "light theme `{}`: base ({}) must be brighter than text ({})",
                    theme.name,
                    b,
                    t
                );
            } else {
                assert!(
                    b < t,
                    "dark theme `{}`: base ({}) must be darker than text ({})",
                    theme.name,
                    b,
                    t
                );
            }
        }
    }
}
