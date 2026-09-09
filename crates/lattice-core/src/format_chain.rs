//! Who formats a range — an ordered chain of typed providers.
//!
//! Three *intents* (`indent`, `reflow`, `reformat`) each resolve a
//! [`ProviderChain`]; the first provider that is available and returns a
//! result wins. See `docs/dev/architecture/text-reflow.md` §6.
//!
//! This module is the **vocabulary only** — parsing, formatting and the
//! ordering. Resolution lives in the host (RF.5), because it needs the
//! LSP client, the process runner and the buffer.
//!
//! ## Why not `formatprg` / `equalprg`
//!
//! Those are one string slot each with a precedence order hardcoded in
//! Rust. Every new placement — "prettier for markdown but the server for
//! TypeScript", or a WASM plugin supplying a formatter — is then a host
//! patch. The whole field converged on typed ordered lists instead
//! (conform.nvim's `formatters_by_ft` + `lsp_format`, Zed's `formatter:`
//! union, Helix's `[language.formatter]`), and paramount #2 is the
//! reason to follow: a chain that is data can be extended by a plugin,
//! and an `if` in `do_format_request` cannot.

use std::fmt;

/// One rung of a [`ProviderChain`].
///
/// The set is open at the edges (`External`, `Plugin`) and closed in the
/// middle, which is what lets `parse` reject a typo like `nativ` while
/// still admitting formatters the editor has never heard of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatProvider {
    /// The built-in engine for the intent: the tree-sitter indent engine
    /// for `indent`, the `textwidth` reflow engine for `reflow`. Not
    /// meaningful for `reformat`, where it resolves to nothing and the
    /// chain moves on.
    Native,
    /// `textDocument/formatting` or `rangeFormatting` from an attached
    /// server.
    ///
    /// **Eligible in any chain, default only in `reformat`.** The
    /// protocol has no indent-only and no reflow request, so putting
    /// this in `indent` or `reflow` gets you a *reformat* — which is a
    /// legitimate thing to want and a bad default, because in the common
    /// case (rustfmt's `wrap_comments` is off by default, prettier's
    /// `proseWrap` defaults to `preserve`) it silently does nothing to a
    /// comment paragraph. text-reflow.md §5.
    Lsp,
    /// The built-in per-language formatter table
    /// (`lattice_format::FormatterSpec::for_lang`) — rustfmt, prettier,
    /// black, gofmt and peers, each probed on `PATH` and skipped when
    /// absent.
    LangDefault,
    /// A user-named external filter, as a command line:
    /// `external:prettier --stdin-filepath %`. The replacement for
    /// `formatprg`, and for the `equalprg` that was never implemented.
    External(String),
    /// A formatter contributed by a WASM plugin, by id.
    Plugin(String),
}

impl FormatProvider {
    /// Canonical string form. Round-trips with [`Self::parse`].
    pub fn label(&self) -> String {
        match self {
            FormatProvider::Native => "native".to_string(),
            FormatProvider::Lsp => "lsp".to_string(),
            FormatProvider::LangDefault => "lang-default".to_string(),
            FormatProvider::External(cmd) => format!("external:{cmd}"),
            FormatProvider::Plugin(id) => format!("plugin:{id}"),
        }
    }

    /// Parse one rung.
    ///
    /// The prefixed forms carry a payload; the bare forms are a closed
    /// set, so an unrecognised bare word is an error rather than being
    /// silently taken for an external command. That distinction is the
    /// point of the `external:` prefix existing at all — without it,
    /// `:set format.reformat=nativ` would install a chain that tries to
    /// run a program called `nativ`.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if let Some(cmd) = s.strip_prefix("external:") {
            let cmd = cmd.trim();
            if cmd.is_empty() {
                return Err("`external:` needs a command line, e.g. \
                            `external:prettier --stdin-filepath %`"
                    .to_string());
            }
            return Ok(FormatProvider::External(cmd.to_string()));
        }
        if let Some(id) = s.strip_prefix("plugin:") {
            let id = id.trim();
            if id.is_empty() {
                return Err("`plugin:` needs a plugin id, e.g. `plugin:my-formatter`".to_string());
            }
            return Ok(FormatProvider::Plugin(id.to_string()));
        }
        match s {
            "native" => Ok(FormatProvider::Native),
            "lsp" => Ok(FormatProvider::Lsp),
            "lang-default" => Ok(FormatProvider::LangDefault),
            other => Err(format!(
                "unknown formatter `{other}` — expected one of \
                 native, lsp, lang-default, external:<command>, plugin:<id>"
            )),
        }
    }
}

impl fmt::Display for FormatProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

/// An ordered list of providers for one intent.
///
/// Written comma-separated: `:set format.reformat=lsp,lang-default`.
///
/// **A command line in an `external:` rung may not contain a comma** —
/// the comma is the chain separator and there is no escape. This is a
/// deliberate limit rather than an oversight: the alternative is a
/// quoting grammar inside a `:set` value, which is a lot of machinery
/// for a case (`--config a,b`) that a one-line wrapper script solves.
/// The error message says so when it bites.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderChain(pub Vec<FormatProvider>);

impl ProviderChain {
    pub fn new(providers: Vec<FormatProvider>) -> Self {
        ProviderChain(providers)
    }

    /// The single-provider chain, for the common defaults.
    pub fn of(provider: FormatProvider) -> Self {
        ProviderChain(vec![provider])
    }

    pub fn iter(&self) -> std::slice::Iter<'_, FormatProvider> {
        self.0.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Canonical string form. Round-trips with [`Self::parse`].
    pub fn label(&self) -> String {
        self.0
            .iter()
            .map(FormatProvider::label)
            .collect::<Vec<_>>()
            .join(",")
    }

    /// Parse a comma-separated chain.
    ///
    /// The empty string is a valid EMPTY chain, not an error: it is how
    /// a user says "this intent does nothing here", and rejecting it
    /// would leave no way to express that short of a sentinel value.
    /// An empty chain reports "nothing configured" at resolution rather
    /// than falling back to a default the user just removed.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(ProviderChain(Vec::new()));
        }
        let mut out = Vec::new();
        for part in s.split(',') {
            if part.trim().is_empty() {
                return Err(
                    "empty entry in the chain — write `lsp,native`, not `lsp,,native`".to_string(),
                );
            }
            out.push(FormatProvider::parse(part)?);
        }
        Ok(ProviderChain(out))
    }
}

impl fmt::Display for ProviderChain {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn every_provider_round_trips() {
        for p in [
            FormatProvider::Native,
            FormatProvider::Lsp,
            FormatProvider::LangDefault,
            FormatProvider::External("prettier --stdin-filepath %".to_string()),
            FormatProvider::Plugin("my-fmt".to_string()),
        ] {
            assert_eq!(FormatProvider::parse(&p.label()).unwrap(), p);
        }
    }

    #[test]
    fn a_chain_round_trips() {
        let c = ProviderChain::parse("lsp,lang-default,native").unwrap();
        assert_eq!(c.len(), 3);
        assert_eq!(ProviderChain::parse(&c.label()).unwrap(), c);
    }

    /// The reason `external:` is a prefix rather than "anything we do
    /// not recognise". Without it a typo installs a chain that tries to
    /// execute a program named after the typo, and the failure surfaces
    /// as "no such file or directory" at format time rather than as a
    /// rejected `:set` at the moment the user made the mistake.
    #[test]
    fn a_bare_typo_is_rejected_rather_than_taken_for_a_command() {
        let err = FormatProvider::parse("nativ").unwrap_err();
        assert!(
            err.contains("native"),
            "the message must name the forms: {err}"
        );
        assert!(err.contains("external:<command>"), "{err}");
    }

    #[test]
    fn an_external_rung_keeps_its_whole_command_line() {
        let p = FormatProvider::parse("external:prettier --stdin-filepath %").unwrap();
        assert_eq!(
            p,
            FormatProvider::External("prettier --stdin-filepath %".to_string())
        );
    }

    #[test]
    fn whitespace_around_entries_is_tolerated() {
        assert_eq!(
            ProviderChain::parse(" lsp , native ").unwrap(),
            ProviderChain::new(vec![FormatProvider::Lsp, FormatProvider::Native])
        );
    }

    /// An empty chain is "this intent does nothing here", which a user
    /// must be able to say. Erroring would leave the intent expressible
    /// only as some sentinel.
    #[test]
    fn the_empty_chain_is_valid_and_means_nothing_runs() {
        let c = ProviderChain::parse("").unwrap();
        assert!(c.is_empty());
        assert_eq!(c.label(), "");
    }

    #[test]
    fn a_hole_in_the_chain_is_an_error_not_a_silent_skip() {
        assert!(ProviderChain::parse("lsp,,native").is_err());
    }

    #[test]
    fn empty_payloads_are_rejected_with_an_example() {
        for bad in ["external:", "plugin:", "external:   "] {
            let err = FormatProvider::parse(bad).unwrap_err();
            assert!(err.contains("e.g."), "{bad} must suggest a form: {err}");
        }
    }
}
