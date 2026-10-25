//! A buffer's language id, from its path.
//!
//! One answer for every consumer that keys on "what language is this
//! file": snippet packs (`rust.json`, `typescriptreact.json`), the
//! completion settings under `[completion.per-language.<id>]`, and the
//! snippet picker. Before snippet-integrity SI.5 the completion side and
//! the picker each had their own table and disagreed — `.rb` was `rb` to
//! one and `ruby` to the other, so `ruby.json` never matched.
//!
//! The ids are the LSP specification's (the same names VS Code and the
//! friendly-snippets corpus use), so a snippet pack named for a language
//! finds its files without translation. Two deliberate exceptions:
//! `.txt` is `text` (the plain-text completion defaults are keyed so), and
//! an extension the table does not know is used as-is, lowercased.

use std::path::Path;

/// The language id for a buffer at `path`; `""` for a buffer with no path.
///
/// # Examples
///
/// ```
/// use lattice_core::language::language_id_for_path;
/// use std::path::Path;
///
/// assert_eq!(language_id_for_path(Some(Path::new("src/main.rs"))), "rust");
/// assert_eq!(language_id_for_path(Some(Path::new("App.TSX"))), "typescriptreact");
/// assert_eq!(language_id_for_path(Some(Path::new("Dockerfile"))), "dockerfile");
/// assert_eq!(language_id_for_path(None), "");
/// ```
pub fn language_id_for_path(path: Option<&Path>) -> String {
    let Some(path) = path else {
        return String::new();
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    match name {
        "Makefile" | "makefile" | "GNUmakefile" => return "makefile".into(),
        "Dockerfile" | "Containerfile" => return "dockerfile".into(),
        _ => {}
    }
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return String::new();
    };
    let ext = ext.to_ascii_lowercase();
    let id = match ext.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "go" => "go",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hxx" | "hh" => "cpp",
        "cs" => "csharp",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "swift" => "swift",
        "rb" => "ruby",
        "php" => "php",
        "lua" => "lua",
        "sh" | "bash" | "zsh" => "shellscript",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" => "scss",
        "less" => "less",
        "json" => "json",
        "jsonc" => "jsonc",
        "yaml" | "yml" => "yaml",
        "toml" => "toml",
        "sql" => "sql",
        "ex" | "exs" => "elixir",
        "erl" | "hrl" => "erlang",
        "hs" => "haskell",
        "ml" | "mli" => "ocaml",
        "dart" => "dart",
        "r" => "r",
        "vim" => "vim",
        "zig" => "zig",
        "nix" => "nix",
        "tex" => "latex",
        "txt" => "text",
        other => return other.to_string(),
    };
    id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(path: &str) -> String {
        language_id_for_path(Some(Path::new(path)))
    }

    #[test]
    fn common_extensions_map_to_lsp_ids() {
        assert_eq!(id("a.rb"), "ruby");
        assert_eq!(id("a.sh"), "shellscript");
        assert_eq!(id("a.zsh"), "shellscript");
        assert_eq!(id("a.jsx"), "javascriptreact");
        assert_eq!(id("a.tsx"), "typescriptreact");
        assert_eq!(id("a.txt"), "text");
        assert_eq!(id("notes.markdown"), "markdown");
    }

    #[test]
    fn extensions_are_case_insensitive_and_unknown_ones_pass_through() {
        assert_eq!(id("MAIN.RS"), "rust");
        assert_eq!(id("a.Foo"), "foo");
    }

    #[test]
    fn some_files_are_known_by_name() {
        assert_eq!(id("build/Makefile"), "makefile");
        assert_eq!(id("Dockerfile"), "dockerfile");
    }

    #[test]
    fn no_path_or_no_extension_is_empty() {
        assert_eq!(language_id_for_path(None), "");
        assert_eq!(id("LICENSE"), "");
    }
}
