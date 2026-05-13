//! Ratatui-side icon adapter (DESIGN.md §5.6.7).
//!
//! The path → (glyph, [`IconColor`]) lookup table lives in
//! `lattice_core::ui::icons`. This module is the renderer-specific
//! adapter that maps `IconColor` to ratatui `Color`/`Style` and
//! applies theme overrides for hidden files / directories.

use std::path::Path;

use lattice_core::ui::icons::{IconColor, entry_visual};
use ratatui::style::{Color, Style};

use crate::theme::Theme;

/// Returns `(glyph, style)` for a directory or file entry. Delegates
/// the path → glyph + colour lookup to `lattice_core::ui::icons`,
/// then maps the renderer-neutral [`IconColor`] to a ratatui `Style`,
/// applying the theme's hidden-file override for dotfiles.
pub fn icon_for_entry(
    path: &Path,
    is_dir: bool,
    nerd_fonts: bool,
    theme: &Theme,
) -> (&'static str, Style) {
    let (glyph, icon_color) = entry_visual(path, is_dir, nerd_fonts);
    if is_dir {
        return (glyph, theme.file_tree_dir_style);
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let is_hidden = name.starts_with('.');
    let color = to_ratatui_color(icon_color);
    let base_style = Style::new().fg(color);
    let style = if is_hidden {
        theme.file_tree_hidden_style
    } else {
        base_style
    };
    (glyph, style)
}

fn to_ratatui_color(c: IconColor) -> Color {
    match c {
        IconColor::Rgb(rgb) => Color::from_u32(rgb),
        IconColor::Reset => Color::Reset,
        IconColor::Yellow => Color::Yellow,
        IconColor::DarkGray => Color::DarkGray,
        IconColor::Blue => Color::Blue,
        IconColor::Cyan => Color::Cyan,
        IconColor::Green => Color::Green,
        IconColor::White => Color::White,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use std::path::PathBuf;

    fn theme() -> Theme {
        Theme::default()
    }

    #[test]
    fn directory_returns_dir_style() {
        let (glyph, style) = icon_for_entry(&PathBuf::from("src"), true, true, &theme());
        assert_eq!(glyph, "󰉋 ");
        assert_eq!(style, theme().file_tree_dir_style);
    }

    #[test]
    fn nerd_fonts_false_returns_bmp_glyph_for_dir() {
        let (glyph, style) = icon_for_entry(&PathBuf::from("src"), true, false, &theme());
        assert_eq!(glyph, "▸ ");
        assert_eq!(style, theme().file_tree_dir_style);
    }

    #[test]
    fn rust_file_gets_orange_glyph() {
        let (glyph, style) = icon_for_entry(&PathBuf::from("main.rs"), false, true, &theme());
        assert_eq!(glyph, "󱘗 ");
        assert_eq!(style.fg, Some(Color::from_u32(0xDEA584)));
    }

    #[test]
    fn hidden_file_uses_hidden_style() {
        let (_, style) = icon_for_entry(&PathBuf::from(".gitignore"), false, true, &theme());
        assert_eq!(style, theme().file_tree_hidden_style);
    }

    #[test]
    fn unknown_ext_falls_back_to_default_file_glyph() {
        let (glyph, _) = icon_for_entry(&PathBuf::from("binary.bin"), false, true, &theme());
        assert_eq!(glyph, " ");
    }

    #[test]
    fn dockerfile_exact_name_match() {
        let (glyph, style) = icon_for_entry(&PathBuf::from("Dockerfile"), false, true, &theme());
        assert_eq!(glyph, "󰡨 ");
        assert_eq!(style.fg, Some(Color::from_u32(0x458EE6)));
    }

    #[test]
    fn makefile_exact_name_match() {
        let (glyph, _) = icon_for_entry(&PathBuf::from("Makefile"), false, true, &theme());
        assert_eq!(glyph, " ");
    }

    #[test]
    fn typescript_tsx_gets_distinct_color() {
        let (_, ts_style) = icon_for_entry(&PathBuf::from("app.ts"), false, true, &theme());
        let (_, tsx_style) = icon_for_entry(&PathBuf::from("App.tsx"), false, true, &theme());
        assert_ne!(ts_style.fg, tsx_style.fg);
    }

    #[test]
    fn image_extensions_resolve() {
        for ext in &["png", "jpg", "gif", "webp", "svg"] {
            let path = PathBuf::from(format!("img.{ext}"));
            let (glyph, _) = icon_for_entry(&path, false, true, &theme());
            assert!(!glyph.is_empty(), "no glyph for .{ext}");
        }
    }

    #[test]
    fn archive_extensions_resolve() {
        for ext in &["zip", "tar", "gz", "7z", "rar"] {
            let path = PathBuf::from(format!("archive.{ext}"));
            let (glyph, _) = icon_for_entry(&path, false, true, &theme());
            assert!(!glyph.is_empty(), "no glyph for .{ext}");
        }
    }

    #[test]
    fn nerd_fonts_false_returns_bmp_glyph_for_file() {
        // Source code (incl. `.rs`) falls into the default bucket
        // → middle dot. Colour still discriminates by language.
        let (glyph, style) = icon_for_entry(&PathBuf::from("main.rs"), false, false, &theme());
        assert_eq!(glyph, "· ");
        assert_eq!(style.fg, Some(Color::from_u32(0xDEA584)));
    }

    #[test]
    fn nerd_fonts_false_picks_per_family_glyph() {
        assert_eq!(
            icon_for_entry(&PathBuf::from("Cargo.toml"), false, false, &theme()).0,
            "◆ "
        );
        assert_eq!(
            icon_for_entry(&PathBuf::from("README.md"), false, false, &theme()).0,
            "≡ "
        );
        assert_eq!(
            icon_for_entry(&PathBuf::from("logo.png"), false, false, &theme()).0,
            "◇ "
        );
        assert_eq!(
            icon_for_entry(&PathBuf::from("dist.zip"), false, false, &theme()).0,
            "■ "
        );
        assert_eq!(
            icon_for_entry(&PathBuf::from("song.mp3"), false, false, &theme()).0,
            "♪ "
        );
    }

    #[test]
    fn graphql_gets_pink_color() {
        let (_, style) = icon_for_entry(&PathBuf::from("schema.graphql"), false, true, &theme());
        assert_eq!(style.fg, Some(Color::from_u32(0xE535AB)));
    }

    #[test]
    fn license_exact_name_match() {
        let (glyph, _) = icon_for_entry(&PathBuf::from("LICENSE"), false, true, &theme());
        assert_eq!(glyph, " ");
    }
}
