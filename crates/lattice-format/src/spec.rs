//! What to run, per language.

use lattice_syntax::Lang;

/// How to invoke one external formatter.
///
/// Every formatter here reads the buffer on **stdin** and writes the
/// result to **stdout**. That is deliberate and not merely convenient:
/// a formatter that rewrites the file in place cannot be run against an
/// unsaved buffer, and formatting only what is on disk is the wrong
/// behaviour for an editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatterSpec {
    /// Program name, resolved through `PATH`.
    pub program: &'static str,
    /// Fixed arguments.
    pub args: &'static [&'static str],
    /// When set, the buffer's path is appended as
    /// `<flag>=<path>`. Several formatters need the filename to pick a
    /// parser or find a config file even when reading stdin
    /// (`prettier --stdin-filepath`, `clang-format
    /// --assume-filename`).
    pub filename_flag: Option<&'static str>,
}

impl FormatterSpec {
    /// The default formatter for `lang`, or `None` when there is no
    /// obvious choice.
    ///
    /// **This table is a starting point, not a contract.** Formatter
    /// CLIs change flags across versions, and houses disagree about
    /// which formatter to use at all. Every entry is one line to
    /// correct, and `formatprg` overrides the whole thing per buffer.
    /// Absent entries are not gaps to be filled on principle — a
    /// language with no consensus formatter is better served by the
    /// user naming one.
    pub fn for_lang(lang: Lang) -> Option<Self> {
        let (program, args, filename_flag) = match lang {
            Lang::Rust => ("rustfmt", &["--emit", "stdout"][..], None),
            Lang::Go => ("gofmt", &[][..], None),
            Lang::Python => ("black", &["-q", "-"][..], None),
            Lang::Lua => ("stylua", &["-"][..], None),
            Lang::Bash => ("shfmt", &[][..], None),
            Lang::Toml => ("taplo", &["format", "-"][..], None),
            Lang::C | Lang::Cpp | Lang::Java => {
                ("clang-format", &[][..], Some("--assume-filename"))
            }
            Lang::JavaScript
            | Lang::TypeScript
            | Lang::Tsx
            | Lang::Css
            | Lang::Html
            | Lang::Json
            | Lang::Yaml
            | Lang::Markdown => ("prettier", &[][..], Some("--stdin-filepath")),
            // No consensus formatter: Ruby is split between rubocop
            // and standardrb, SQL between a dozen tools with
            // incompatible opinions, and `Plain` has nothing to format.
            // WIT has no stdin-oriented consensus formatter — `wasm-tools
            // component wit` reformats a file in place, which is a different
            // shape from the pipe every arm above uses.
            Lang::Ruby | Lang::Sql | Lang::Plain | Lang::Wit => return None,
            // No built-in formatter for a language the editor learned
            // about at runtime. `formatprg` still works — it overrides
            // this table per buffer and is the documented answer for
            // any language without a consensus formatter.
            Lang::Plugin(_) => return None,
        };
        Some(Self {
            program,
            args,
            filename_flag,
        })
    }

    /// Parse a user-supplied `formatprg` / `equalprg` string.
    ///
    /// Whitespace-split, first word is the program. Leaked to
    /// `'static` because [`FormatterSpec`] holds `&'static str` for the
    /// built-in table's sake; a handful of leaked option strings over a
    /// session is not a leak worth a lifetime parameter through the
    /// whole crate.
    ///
    /// Returns `None` for an empty or whitespace-only string, which is
    /// the "not configured" signal rather than an error.
    pub fn parse(s: &str) -> Option<Self> {
        let mut words = s.split_whitespace();
        let program: &'static str = Box::leak(words.next()?.to_string().into_boxed_str());
        let args: Vec<&'static str> = words
            .map(|w| &*Box::leak(w.to_string().into_boxed_str()))
            .collect();
        Some(Self {
            program,
            args: Box::leak(args.into_boxed_slice()),
            filename_flag: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_languages_map_to_their_usual_formatter() {
        assert_eq!(
            FormatterSpec::for_lang(Lang::Rust).unwrap().program,
            "rustfmt"
        );
        assert_eq!(FormatterSpec::for_lang(Lang::Go).unwrap().program, "gofmt");
        assert_eq!(
            FormatterSpec::for_lang(Lang::TypeScript).unwrap().program,
            "prettier"
        );
    }

    #[test]
    fn languages_without_consensus_have_no_default() {
        // Not gaps: naming one would pick a side in an argument the
        // editor has no stake in.
        assert!(FormatterSpec::for_lang(Lang::Ruby).is_none());
        assert!(FormatterSpec::for_lang(Lang::Sql).is_none());
        assert!(FormatterSpec::for_lang(Lang::Plain).is_none());
    }

    #[test]
    fn formatters_needing_a_filename_carry_the_flag() {
        assert_eq!(
            FormatterSpec::for_lang(Lang::Cpp).unwrap().filename_flag,
            Some("--assume-filename")
        );
        assert_eq!(
            FormatterSpec::for_lang(Lang::Json).unwrap().filename_flag,
            Some("--stdin-filepath")
        );
        assert_eq!(
            FormatterSpec::for_lang(Lang::Rust).unwrap().filename_flag,
            None
        );
    }

    #[test]
    fn formatprg_parses_into_program_and_args() {
        let spec = FormatterSpec::parse("  my-fmt --stdin  --width 80 ").unwrap();
        assert_eq!(spec.program, "my-fmt");
        assert_eq!(spec.args, &["--stdin", "--width", "80"]);
    }

    #[test]
    fn an_empty_formatprg_is_not_configured_rather_than_an_error() {
        assert!(FormatterSpec::parse("").is_none());
        assert!(FormatterSpec::parse("   ").is_none());
    }
}
