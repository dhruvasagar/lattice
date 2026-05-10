//! Renderer-agnostic file-type icon resolver (DESIGN.md §5.6.7).
//!
//! Returns a Nerd Fonts v3 glyph + a renderer-neutral [`IconColor`]
//! for any file or directory path. The glyph is plain text -- the
//! `lattice-file-tree` render path embeds it directly in the rope so
//! the model stays usable from any renderer. The colour is mapped
//! to the renderer's native style at draw time (ratatui `Color`,
//! GPUI `Hsla`, etc.).
//!
//! Resolution order: exact filename → extension → default. When
//! `nerd_fonts` is false the glyph is `""` and only the colour
//! carries visual differentiation.

use std::path::Path;

/// Renderer-neutral colour. Concrete renderers (ratatui, GPUI, ...)
/// map the named variants to their own palette and `Rgb` straight
/// through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconColor {
    /// Truecolor RGB packed as `0xRRGGBB`.
    Rgb(u32),
    /// "default foreground" -- the renderer uses its current style.
    Reset,
    Yellow,
    DarkGray,
    Blue,
    Cyan,
    Green,
    White,
}

/// Resolve `(glyph, colour)` for a directory or file entry.
///
/// `is_dir` selects the directory branch (folder glyph regardless
/// of name); for files the lookup falls through name → extension →
/// default. `nerd_fonts == false` returns an empty glyph with the
/// colour still resolved so the renderer can still tint by type.
pub fn entry_visual(path: &Path, is_dir: bool, nerd_fonts: bool) -> (&'static str, IconColor) {
    if is_dir {
        let glyph = if nerd_fonts { "󰉋 " } else { "" };
        return (glyph, IconColor::Blue);
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if nerd_fonts {
        nerd_glyph_and_color(name, ext)
    } else {
        ("", ext_color(name, ext))
    }
}

/// Glyph-only resolver. Used by the file-tree / oil rope renderers
/// that embed the glyph in editable text and let the renderer apply
/// styling spans. Equivalent to the first half of `entry_visual`.
pub fn glyph_for_entry(path: &Path, is_dir: bool, nerd_fonts: bool) -> &'static str {
    entry_visual(path, is_dir, nerd_fonts).0
}

fn nerd_glyph_and_color(name: &str, ext: &str) -> (&'static str, IconColor) {
    match name {
        "Makefile" | "makefile" | "GNUmakefile" | "BSDmakefile" | "Makefile.am" => {
            return (" ", IconColor::Rgb(0x6D8086));
        }
        "CMakeLists.txt" => return (" ", IconColor::Rgb(0x6D8086)),
        "Dockerfile" | "dockerfile" | "Containerfile" | "containerfile" => {
            return ("󰡨 ", IconColor::Rgb(0x458EE6));
        }
        "docker-compose.yml" | "docker-compose.yaml" | "compose.yml" | "compose.yaml" => {
            return ("󰡨 ", IconColor::Rgb(0x458EE6));
        }
        ".dockerignore" => return ("󰡨 ", IconColor::DarkGray),
        ".gitignore" | ".gitattributes" | ".gitmodules" | ".gitconfig" | ".gitmessage" => {
            return ("󰒓 ", IconColor::Rgb(0x41535B));
        }
        "LICENSE" | "LICENCE" | "LICENSE.txt" | "LICENCE.txt" | "LICENSE.md" | "LICENCE.md" => {
            return (" ", IconColor::Yellow);
        }
        ".editorconfig" => return (" ", IconColor::DarkGray),
        ".env" | ".envrc" => return (" ", IconColor::Rgb(0xFAF743)),
        "package.json" => return (" ", IconColor::Rgb(0xE8274B)),
        "package-lock.json" => return (" ", IconColor::DarkGray),
        ".npmrc" | ".nvmrc" | ".node-version" => return (" ", IconColor::Rgb(0xE8274B)),
        "tsconfig.json" | "tsconfig.base.json" | "tsconfig.build.json" => {
            return (" ", IconColor::Rgb(0x519ABA));
        }
        "jsconfig.json" => return (" ", IconColor::Rgb(0xCBCB41)),
        ".eslintrc" | ".eslintrc.json" | ".eslintrc.js" | ".eslintrc.yml" | ".eslintrc.cjs" => {
            return (" ", IconColor::Rgb(0x4B32C3));
        }
        ".prettierrc" | ".prettierrc.json" | ".prettierrc.js" | ".prettierrc.yml" => {
            return (" ", IconColor::Rgb(0x56B3B4));
        }
        "vite.config.ts" | "vite.config.js" => return (" ", IconColor::Rgb(0xBD34FE)),
        "vitest.config.ts" | "vitest.config.js" => return (" ", IconColor::Rgb(0xBD34FE)),
        "webpack.config.js" | "webpack.config.ts" => return ("󰜫 ", IconColor::Rgb(0x519ABA)),
        "next.config.js" | "next.config.ts" | "next.config.mjs" => {
            return (" ", IconColor::White);
        }
        "nuxt.config.ts" | "nuxt.config.js" => return (" ", IconColor::Rgb(0x00C58E)),
        "svelte.config.js" | "svelte.config.ts" => return (" ", IconColor::Rgb(0xFF3E00)),
        "astro.config.mjs" | "astro.config.ts" => return (" ", IconColor::Rgb(0xE8274B)),
        "tailwind.config.js" | "tailwind.config.ts" | "tailwind.config.cjs" => {
            return (" ", IconColor::Rgb(0x38BDF8));
        }
        "postcss.config.js" | "postcss.config.cjs" => return (" ", IconColor::DarkGray),
        "requirements.txt" | "requirements-dev.txt" | "requirements-test.txt" => {
            return (" ", IconColor::Rgb(0xFFBC03));
        }
        "Pipfile" | "Pipfile.lock" => return (" ", IconColor::Rgb(0xFFBC03)),
        "pyproject.toml" => return (" ", IconColor::Rgb(0xFFBC03)),
        "setup.py" | "setup.cfg" | "MANIFEST.in" => return (" ", IconColor::Rgb(0xFFBC03)),
        "uv.lock" => return (" ", IconColor::Rgb(0xDE5FE9)),
        "Gemfile" | "Gemfile.lock" => return (" ", IconColor::Rgb(0xFF5252)),
        "Rakefile" => return (" ", IconColor::Rgb(0xFF5252)),
        "go.mod" | "go.sum" | "go.work" => return (" ", IconColor::Rgb(0x519ABA)),
        "Cargo.toml" | "Cargo.lock" => return ("󱘗 ", IconColor::Rgb(0xFF8C00)),
        "Brewfile" | "Brewfile.lock.json" => return (" ", IconColor::Rgb(0xFBB040)),
        "Justfile" | "justfile" => return (" ", IconColor::Rgb(0x6D8086)),
        "Procfile" => return (" ", IconColor::DarkGray),
        _ => {}
    }

    match ext {
        "rs" => ("󱘗 ", IconColor::Rgb(0xDEA584)),
        "c" | "h" => (" ", IconColor::Rgb(0xA8B9CC)),
        "cpp" | "cxx" | "cc" | "c++" | "hpp" | "hxx" | "hh" | "h++" => {
            (" ", IconColor::Rgb(0xF34B7D))
        }
        "cs" | "csx" => ("󰌛 ", IconColor::Rgb(0x596706)),
        "java" => (" ", IconColor::Rgb(0xCC3E44)),
        "kt" | "kts" => (" ", IconColor::Rgb(0x7F52FF)),
        "swift" => (" ", IconColor::Rgb(0xE37933)),
        "go" => (" ", IconColor::Rgb(0x519ABA)),
        "zig" => (" ", IconColor::Rgb(0xF7A41D)),
        "nim" | "nims" | "nimble" => (" ", IconColor::Rgb(0xF3D400)),
        "v" | "vv" => (" ", IconColor::Rgb(0x5D87BF)),
        "odin" => (" ", IconColor::Rgb(0x60AEEF)),
        "asm" | "s" | "S" | "nasm" => (" ", IconColor::DarkGray),

        "py" | "pyw" | "pyi" => (" ", IconColor::Rgb(0xFFBC03)),
        "rb" | "erb" | "gemspec" | "rake" => (" ", IconColor::Rgb(0xFF5252)),
        "php" | "php3" | "php4" | "php5" | "phtml" => (" ", IconColor::Rgb(0xA074C4)),
        "lua" => (" ", IconColor::Rgb(0x51A0CF)),
        "pl" | "pm" | "t" | "pod" => (" ", IconColor::Rgb(0x519ABA)),
        "r" | "rmd" | "rnw" | "rproj" => (" ", IconColor::Rgb(0x2266BA)),
        "jl" => (" ", IconColor::Rgb(0xA270BA)),
        "dart" => (" ", IconColor::Rgb(0x03589C)),
        "groovy" | "gvy" => (" ", IconColor::Rgb(0x4298B8)),
        "tcl" | "tk" => (" ", IconColor::DarkGray),

        "hs" | "lhs" => (" ", IconColor::Rgb(0xA074C4)),
        "ex" | "exs" | "heex" | "leex" => (" ", IconColor::Rgb(0xA074C4)),
        "erl" | "hrl" => (" ", IconColor::Rgb(0xB83998)),
        "clj" | "cljs" | "cljc" | "edn" => (" ", IconColor::Rgb(0x8DC149)),
        "scala" | "sbt" | "sc" => (" ", IconColor::Rgb(0xCC3E44)),
        "ml" | "mli" | "mll" | "mly" => (" ", IconColor::Rgb(0xE37933)),
        "fs" | "fsi" | "fsx" | "fsproj" => (" ", IconColor::Rgb(0x519ABA)),
        "elm" => (" ", IconColor::Rgb(0x1293D8)),
        "cr" => (" ", IconColor::DarkGray),
        "d" | "di" => (" ", IconColor::Rgb(0xB03931)),
        "purs" => (" ", IconColor::Rgb(0x14213D)),
        "rkt" | "rktl" | "rktd" => (" ", IconColor::Rgb(0x9F1D20)),
        "lisp" | "lsp" | "el" | "elc" => (" ", IconColor::Rgb(0x9B59B6)),
        "scm" | "ss" => (" ", IconColor::Rgb(0x9B59B6)),

        "sh" | "bash" | "zsh" | "ksh" | "csh" | "tcsh" => (" ", IconColor::Rgb(0x4D5A5E)),
        "fish" => (" ", IconColor::Rgb(0x4D5A5E)),
        "ps1" | "psm1" | "psd1" | "ps1xml" => ("󰨊 ", IconColor::Rgb(0x4273CA)),
        "bat" | "cmd" => (" ", IconColor::Rgb(0xC1F12E)),

        "html" | "htm" | "xhtml" | "shtml" => (" ", IconColor::Rgb(0xE44D26)),
        "css" => (" ", IconColor::Rgb(0x42A5F5)),
        "scss" | "sass" => (" ", IconColor::Rgb(0xF55385)),
        "less" => (" ", IconColor::Rgb(0x563D7C)),
        "js" | "mjs" | "cjs" => (" ", IconColor::Rgb(0xCBCB41)),
        "ts" => (" ", IconColor::Rgb(0x519ABA)),
        "tsx" => (" ", IconColor::Rgb(0x1354BF)),
        "jsx" => (" ", IconColor::Rgb(0x519ABA)),
        "vue" => (" ", IconColor::Rgb(0x8DC149)),
        "svelte" => (" ", IconColor::Rgb(0xFF3E00)),
        "astro" => (" ", IconColor::Rgb(0xE8274B)),
        "njk" => ("󰗀 ", IconColor::Rgb(0x1BA354)),
        "hbs" | "handlebars" | "mustache" => (" ", IconColor::Rgb(0xF7931E)),
        "ejs" => (" ", IconColor::Rgb(0xA91E50)),
        "pug" | "jade" => (" ", IconColor::Rgb(0xE44B23)),
        "coffee" | "litcoffee" => (" ", IconColor::Rgb(0xCBCB41)),
        "wasm" => (" ", IconColor::Rgb(0x5C4CDB)),
        "wat" => (" ", IconColor::Rgb(0x5C4CDB)),

        "json" | "jsonc" | "json5" => (" ", IconColor::Rgb(0xCBCB41)),
        "yaml" | "yml" => (" ", IconColor::Rgb(0x6D8086)),
        "toml" => (" ", IconColor::Rgb(0x9C4221)),
        "xml" | "xsl" | "xslt" | "xsd" | "dtd" | "rss" | "atom" | "plist" => {
            ("󰗀 ", IconColor::Rgb(0xE37933))
        }
        "ini" | "cfg" | "conf" | "config" | "properties" | "prop" | "rc" => {
            (" ", IconColor::Rgb(0x6D8086))
        }
        "env" | "envrc" | "dotenv" => (" ", IconColor::Rgb(0xFAF743)),
        "csv" | "tsv" => (" ", IconColor::Rgb(0x89E051)),

        "sql" | "mysql" | "pgsql" | "psql" | "plsql" | "hql" => {
            (" ", IconColor::Rgb(0xDAD8D8))
        }
        "sqlite" | "sqlite3" | "db" => (" ", IconColor::Rgb(0x003B57)),
        "prisma" => (" ", IconColor::Rgb(0x5A67D8)),
        "graphql" | "gql" => (" ", IconColor::Rgb(0xE535AB)),

        "md" | "mdx" | "markdown" => ("󰍔 ", IconColor::White),
        "rst" => (" ", IconColor::White),
        "txt" | "text" => (" ", IconColor::White),
        "pdf" => (" ", IconColor::Rgb(0xB30B00)),
        "doc" | "docx" | "odt" | "rtf" => (" ", IconColor::Rgb(0x295394)),
        "xls" | "xlsx" | "ods" | "numbers" => (" ", IconColor::Rgb(0x217346)),
        "ppt" | "pptx" | "odp" | "keynote" => (" ", IconColor::Rgb(0xB7472A)),
        "tex" | "ltx" | "sty" | "bib" | "cls" => (" ", IconColor::DarkGray),
        "org" => (" ", IconColor::Rgb(0x77AA99)),
        "adoc" | "asciidoc" => (" ", IconColor::White),
        "man" | "roff" | "groff" => (" ", IconColor::DarkGray),
        "ipynb" => (" ", IconColor::Rgb(0xF37726)),

        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tiff" | "tif" | "ico" | "psd"
        | "xcf" | "heic" | "heif" | "avif" => (" ", IconColor::Rgb(0xA074C4)),
        "svg" | "svgz" => ("󰜡 ", IconColor::Rgb(0xFFB13B)),
        "ai" | "eps" => (" ", IconColor::Rgb(0xFF7C00)),
        "sketch" | "fig" | "figma" => (" ", IconColor::Rgb(0xFDB300)),

        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "m4v" | "ogv" => {
            (" ", IconColor::Rgb(0xFD971F))
        }
        "mp3" | "ogg" | "flac" | "wav" | "m4a" | "aac" | "wma" | "opus" | "aiff" => {
            ("󰎆 ", IconColor::Rgb(0x00AFFF))
        }

        "zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "lz4" | "7z" | "rar" | "cab"
        | "iso" => (" ", IconColor::Rgb(0xECA517)),
        "deb" | "rpm" | "pkg" | "apk" | "snap" => (" ", IconColor::Rgb(0xECA517)),

        "ttf" | "otf" | "woff" | "woff2" | "eot" => (" ", IconColor::Rgb(0xFFAFF3)),

        "pem" | "key" | "crt" | "cer" | "der" | "p12" | "pfx" | "p8" | "pub" => {
            ("󰌇 ", IconColor::Rgb(0xD4AF37))
        }
        "gpg" | "asc" | "sig" => ("󰌇 ", IconColor::Rgb(0xD4AF37)),

        "exe" | "msi" | "app" => (" ", IconColor::Rgb(0x9F0500)),
        "dll" | "so" | "dylib" | "lib" | "a" => (" ", IconColor::DarkGray),
        "o" | "obj" | "pyc" | "class" => (" ", IconColor::DarkGray),

        "patch" | "diff" => (" ", IconColor::Rgb(0x41535B)),

        "gitignore" | "gitmodules" | "gitattributes" | "gitconfig" => {
            ("󰒓 ", IconColor::Rgb(0x41535B))
        }

        "dockerfile" | "containerfile" => ("󰡨 ", IconColor::Rgb(0x458EE6)),
        "tf" | "tfvars" => (" ", IconColor::Rgb(0x844FBA)),
        "hcl" => (" ", IconColor::Rgb(0x844FBA)),
        "nix" => (" ", IconColor::Rgb(0x7EB7E4)),
        "proto" | "protobuf" => (" ", IconColor::Rgb(0x5BA4CF)),
        "thrift" => (" ", IconColor::DarkGray),
        "k8s" | "helm" => (" ", IconColor::Rgb(0x326CE5)),

        "mk" | "mak" => (" ", IconColor::Rgb(0x6D8086)),
        "cmake" => (" ", IconColor::Rgb(0x6D8086)),
        "bazel" | "bzl" => (" ", IconColor::Rgb(0x76D275)),
        "ninja" => (" ", IconColor::DarkGray),
        "gradle" => (" ", IconColor::Rgb(0x4298B8)),

        "vim" | "vimrc" | "nvimrc" => (" ", IconColor::Rgb(0x019833)),
        "lock" => ("󰌾 ", IconColor::DarkGray),
        "log" => (" ", IconColor::DarkGray),
        "pid" => (" ", IconColor::DarkGray),
        "tmp" | "temp" => (" ", IconColor::DarkGray),
        "bak" | "backup" | "old" => (" ", IconColor::DarkGray),
        "swp" | "swo" => (" ", IconColor::DarkGray),
        "mdump" | "dmp" => (" ", IconColor::DarkGray),

        _ => (" ", IconColor::Reset),
    }
}

fn ext_color(name: &str, ext: &str) -> IconColor {
    match name {
        "Makefile" | "makefile" | "CMakeLists.txt" => return IconColor::Rgb(0x6D8086),
        "Dockerfile" | "dockerfile" => return IconColor::Blue,
        ".gitignore" | ".gitattributes" | ".gitmodules" => return IconColor::DarkGray,
        "LICENSE" | "LICENCE" => return IconColor::Yellow,
        _ => {}
    }
    match ext {
        "rs" => IconColor::Rgb(0xDEA584),
        "c" | "h" | "cpp" | "cxx" | "hpp" => IconColor::Blue,
        "cs" | "java" | "scala" => IconColor::Rgb(0xCC3E44),
        "kt" | "kts" => IconColor::Rgb(0x7F52FF),
        "swift" => IconColor::Rgb(0xE37933),
        "go" => IconColor::Cyan,
        "zig" | "nim" => IconColor::Yellow,
        "py" | "pyw" | "pyi" => IconColor::Rgb(0xFFBC03),
        "rb" | "erb" => IconColor::Rgb(0xFF5252),
        "php" => IconColor::Rgb(0xA074C4),
        "lua" => IconColor::Blue,
        "r" | "jl" => IconColor::Blue,
        "dart" => IconColor::Cyan,
        "hs" | "lhs" => IconColor::Rgb(0xA074C4),
        "ex" | "exs" => IconColor::Rgb(0xA074C4),
        "erl" | "hrl" => IconColor::Rgb(0xB83998),
        "clj" | "cljs" => IconColor::Green,
        "js" | "mjs" | "cjs" | "coffee" => IconColor::Rgb(0xCBCB41),
        "ts" | "tsx" | "jsx" => IconColor::Blue,
        "html" | "htm" => IconColor::Rgb(0xE44D26),
        "css" | "less" => IconColor::Blue,
        "scss" | "sass" => IconColor::Rgb(0xF55385),
        "vue" | "svelte" => IconColor::Green,
        "toml" => IconColor::Yellow,
        "json" | "jsonc" | "json5" => IconColor::Yellow,
        "yaml" | "yml" => IconColor::Green,
        "xml" => IconColor::Rgb(0xE37933),
        "sh" | "bash" | "zsh" | "fish" => IconColor::Green,
        "ps1" | "psm1" => IconColor::Blue,
        "vim" | "vimrc" => IconColor::Green,
        "md" | "mdx" | "rst" | "txt" => IconColor::White,
        "sql" => IconColor::DarkGray,
        "graphql" | "gql" => IconColor::Rgb(0xE535AB),
        "tf" | "hcl" => IconColor::Rgb(0x844FBA),
        "nix" => IconColor::Blue,
        "prisma" => IconColor::Rgb(0x5A67D8),
        "lock" => IconColor::DarkGray,
        "log" => IconColor::DarkGray,
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "bmp" | "webp" | "ico" => {
            IconColor::Rgb(0xA074C4)
        }
        "mp4" | "mkv" | "avi" | "mov" => IconColor::Rgb(0xFD971F),
        "mp3" | "flac" | "wav" | "ogg" => IconColor::Cyan,
        "zip" | "tar" | "gz" | "bz2" | "7z" | "rar" => IconColor::Rgb(0xECA517),
        "pdf" => IconColor::Rgb(0xB30B00),
        _ => IconColor::Reset,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn directory_returns_dir_glyph() {
        let (glyph, _) = entry_visual(&PathBuf::from("src"), true, true);
        assert_eq!(glyph, "󰉋 ");
    }

    #[test]
    fn nerd_fonts_false_returns_empty_glyph_for_dir() {
        let (glyph, _) = entry_visual(&PathBuf::from("src"), true, false);
        assert_eq!(glyph, "");
    }

    #[test]
    fn rust_file_gets_orange_rgb() {
        let (glyph, color) = entry_visual(&PathBuf::from("main.rs"), false, true);
        assert_eq!(glyph, "󱘗 ");
        assert_eq!(color, IconColor::Rgb(0xDEA584));
    }

    #[test]
    fn unknown_ext_falls_back_to_default_glyph() {
        let (glyph, _) = entry_visual(&PathBuf::from("binary.bin"), false, true);
        assert_eq!(glyph, " ");
    }

    #[test]
    fn dockerfile_exact_name_match() {
        let (glyph, color) = entry_visual(&PathBuf::from("Dockerfile"), false, true);
        assert_eq!(glyph, "󰡨 ");
        assert_eq!(color, IconColor::Rgb(0x458EE6));
    }

    #[test]
    fn glyph_only_helper_matches_visual_first_field() {
        let path = PathBuf::from("App.tsx");
        let g = glyph_for_entry(&path, false, true);
        let (g2, _) = entry_visual(&path, false, true);
        assert_eq!(g, g2);
    }
}
