//! Languages this crate knows how to parse.

use std::path::Path;

use crate::plugin_lang::{self, LanguageName};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    Plain,
    Rust,
    Python,
    JavaScript,
    Bash,
    C,
    Cpp,
    Css,
    Go,
    Html,
    Java,
    Json,
    Lua,
    Ruby,
    /// SQL via `tree-sitter-sequel` — general/permissive SQL grammar
    /// supporting multiple dialects (MySQL, PostgreSQL, SQLite).
    Sql,
    Toml,
    TypeScript,
    /// TypeScript + JSX (React) via `tree-sitter-typescript`'s TSX
    /// grammar. Separate from `TypeScript` because the two are
    /// different dialects — TSX adds JSX element syntax on top of
    /// the TypeScript parser.
    Tsx,
    Yaml,
    /// CommonMark + GitHub-flavor markdown via `tree-sitter-md`.
    /// The grammar is intentionally split into block + inline parsers;
    /// the registry holds both as separate `HighlightConfiguration`s
    /// (`markdown` and `markdown_inline`) and the block grammar's
    /// injection queries thread between them. Help buffers also
    /// render through this language.
    Markdown,
    /// A language contributed at runtime by a plugin (LG.2; design
    /// [`plugin-languages.md`](../../../docs/dev/architecture/plugin-languages.md) §2.3).
    ///
    /// The existing variants deliberately stay. Native languages keep
    /// compiler-checked coverage in [`Self::comment_syntax`],
    /// [`crate::major_mode_id_for_lang`] and `FormatSpec::for_lang` —
    /// so adding a bundled language still cannot silently miss its
    /// formatter — and a plugin language is one fallthrough arm at each.
    ///
    /// The payload is the language's *name*, interned, rather than an
    /// index: [`Self::name`] is the key every query lookup already uses,
    /// and it stays a field read. See [`crate::plugin_lang`].
    Plugin(LanguageName),
}

impl Lang {
    /// The compiled-in language with this canonical name, if any.
    ///
    /// Exists so [`crate::plugin_lang::register`] can refuse a name that
    /// would shadow a builtin, and it is written as a match over the
    /// same table [`Self::name`] uses so the two cannot drift.
    pub fn builtin_by_name(name: &str) -> Option<Self> {
        const BUILTINS: &[Lang] = &[
            Lang::Plain,
            Lang::Rust,
            Lang::Python,
            Lang::JavaScript,
            Lang::Bash,
            Lang::C,
            Lang::Cpp,
            Lang::Css,
            Lang::Go,
            Lang::Html,
            Lang::Java,
            Lang::Json,
            Lang::Lua,
            Lang::Ruby,
            Lang::Sql,
            Lang::Toml,
            Lang::TypeScript,
            Lang::Tsx,
            Lang::Yaml,
            Lang::Markdown,
        ];
        BUILTINS.iter().copied().find(|l| l.name() == name).or_else(
            // Not a `Lang` variant, but it IS a registry key: markdown's
            // inline grammar. A plugin claiming it would collide in the
            // config map even though no `Lang` names it.
            || (name == "markdown_inline").then_some(Lang::Markdown),
        )
    }

    /// Detect language from a file path's extension.
    pub fn detect_from_path(path: Option<&Path>) -> Self {
        // Check known shell rc/profile filenames (dotfiles with no extension).
        if let Some(p) = path
            && let Some(name) = p.file_name().and_then(|n| n.to_str())
        {
            let lower = name.to_ascii_lowercase();
            match lower.as_str() {
                ".bashrc" | ".bash_profile" | ".bash_login" | ".bash_logout" | ".zshrc"
                | ".zshenv" | ".zprofile" | ".zlogin" | ".zlogout" | ".profile" | ".shrc"
                | ".kshrc" => return Lang::Bash,
                _ => {}
            }
        }
        match path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("rs") => Lang::Rust,
            Some("py") | Some("pyw") => Lang::Python,
            Some("js") | Some("mjs") | Some("cjs") => Lang::JavaScript,
            Some("sh") | Some("bash") | Some("zsh") | Some("fish") => Lang::Bash,
            Some("c") | Some("h") => Lang::C,
            Some("cpp") | Some("cc") | Some("cxx") | Some("hpp") | Some("hh") | Some("hxx") => {
                Lang::Cpp
            }
            Some("css") => Lang::Css,
            Some("go") => Lang::Go,
            Some("html") | Some("htm") | Some("xhtml") => Lang::Html,
            Some("java") => Lang::Java,
            Some("json") => Lang::Json,
            Some("lua") => Lang::Lua,
            Some("rb") | Some("ruby") => Lang::Ruby,
            Some("sql") => Lang::Sql,
            Some("toml") => Lang::Toml,
            Some("ts") | Some("mts") | Some("cts") => Lang::TypeScript,
            Some("tsx") => Lang::Tsx,
            Some("yaml") | Some("yml") => Lang::Yaml,
            Some("md") | Some("markdown") | Some("mdown") | Some("mkd") => Lang::Markdown,
            // The runtime registry is consulted only AFTER every native
            // arm, so a plugin cannot shadow a bundled language by
            // accident. When nothing is registered this costs one
            // relaxed atomic load — `detect_from_path` runs per hunk in
            // magit's diff highlighting, so the empty case has to be
            // free rather than merely cheap.
            Some(ext) => plugin_lang::resolve_extension(ext).map_or(Lang::Plain, Lang::Plugin),
            None => Lang::Plain,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lang::Plain => "plain",
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::Bash => "bash",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Css => "css",
            Lang::Go => "go",
            Lang::Html => "html",
            Lang::Java => "java",
            Lang::Json => "json",
            Lang::Lua => "lua",
            Lang::Ruby => "ruby",
            Lang::Sql => "sql",
            Lang::Toml => "toml",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::Yaml => "yaml",
            Lang::Markdown => "markdown",
            // The interned name IS the identity — a field read, not a
            // table lookup. See `plugin_lang`.
            Lang::Plugin(n) => n.as_str(),
        }
    }

    /// Canonical name used as a registry key. The block markdown
    /// parser is registered under `"markdown"`; the inline variant
    /// (registered under `"markdown_inline"`) is reached only via
    /// the block grammar's injection queries.
    pub fn name(self) -> &'static str {
        match self {
            Lang::Plain => "plain",
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
            Lang::Bash => "bash",
            Lang::C => "c",
            Lang::Cpp => "cpp",
            Lang::Css => "css",
            Lang::Go => "go",
            Lang::Html => "html",
            Lang::Java => "java",
            Lang::Json => "json",
            Lang::Lua => "lua",
            Lang::Ruby => "ruby",
            Lang::Sql => "sql",
            Lang::Toml => "toml",
            Lang::TypeScript => "typescript",
            Lang::Tsx => "tsx",
            Lang::Yaml => "yaml",
            Lang::Markdown => "markdown",
            // The interned name IS the identity — a field read, not a
            // table lookup. See `plugin_lang`.
            Lang::Plugin(n) => n.as_str(),
        }
    }

    /// N.1.6: per-language comment-leader descriptor for the comment
    /// text objects (`aC` / `iC`). Commentstring-driven — the line
    /// leader is all v1 uses; block delimiters are carried for a
    /// follow-up. Languages with no line comment (markdown, plain)
    /// return `line: None`, so the comment objects no-op there.
    pub fn comment_syntax(self) -> lattice_grammar::CommentSyntax {
        let (line, block): (Option<&str>, Option<(&str, &str)>) = match self {
            Lang::Rust | Lang::JavaScript | Lang::TypeScript | Lang::Tsx => {
                (Some("//"), Some(("/*", "*/")))
            }
            Lang::Python | Lang::Ruby | Lang::Bash | Lang::Yaml | Lang::Toml => (Some("#"), None),
            Lang::Go | Lang::C | Lang::Cpp | Lang::Java | Lang::Sql => {
                (Some("//"), Some(("/*", "*/")))
            }
            Lang::Css => (None, Some(("/*", "*/"))),
            Lang::Html => (None, Some(("<!--", "-->"))),
            Lang::Lua => (Some("--"), Some(("--[[", "]]"))),
            Lang::Json | Lang::Plain | Lang::Markdown => (None, None),
            // LG.3 lets a plugin declare its comment syntax; until
            // then `aC` / `iC` no-op in a plugin language exactly as
            // they do in markdown, rather than guessing a leader.
            Lang::Plugin(_) => (None, None),
        };
        lattice_grammar::CommentSyntax {
            line: line.map(str::to_string),
            block: block.map(|(s, e)| (s.to_string(), e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_rust() {
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("foo.rs"))),
            Lang::Rust
        );
    }

    #[test]
    fn detects_python() {
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("foo.py"))),
            Lang::Python
        );
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("foo.pyw"))),
            Lang::Python
        );
    }

    #[test]
    fn detects_javascript() {
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("foo.js"))),
            Lang::JavaScript
        );
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("foo.mjs"))),
            Lang::JavaScript
        );
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("foo.cjs"))),
            Lang::JavaScript
        );
    }

    #[test]
    fn falls_back_to_plain() {
        assert_eq!(Lang::detect_from_path(None), Lang::Plain);
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("README"))),
            Lang::Plain
        );
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("foo.unknown"))),
            Lang::Plain
        );
    }

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("FOO.RS"))),
            Lang::Rust
        );
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("Foo.PY"))),
            Lang::Python
        );
    }

    #[test]
    fn label_is_distinct_per_lang() {
        assert_eq!(Lang::Plain.label(), "plain");
        assert_eq!(Lang::Rust.label(), "rust");
        assert_eq!(Lang::Python.label(), "python");
        assert_eq!(Lang::JavaScript.label(), "javascript");
        assert_eq!(Lang::Markdown.label(), "markdown");
    }

    #[test]
    fn detects_markdown_extensions() {
        for ext in ["md", "markdown", "mdown", "mkd"] {
            assert_eq!(
                Lang::detect_from_path(Some(&PathBuf::from(format!("README.{ext}")))),
                Lang::Markdown,
                "{ext}"
            );
        }
        // Case-insensitive.
        assert_eq!(
            Lang::detect_from_path(Some(&PathBuf::from("README.MD"))),
            Lang::Markdown
        );
    }
}
