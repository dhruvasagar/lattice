//! Parsed snippet body: the token stream a [`crate::parse::parse`]
//! produces and [`crate::render::render`] walks.
//!
//! Tokens map 1:1 to TextMate / LSP snippet syntax constructs
//! plus a tail `Literal` for plain text runs:
//!
//! | Construct | Token |
//! |---|---|
//! | `foo bar` | [`SnippetToken::Literal`] |
//! | `$1`, `$0`, `$99` | [`SnippetToken::Tabstop`] |
//! | `${1:default text}` | [`SnippetToken::Placeholder`] |
//! | `${1\|opt1,opt2,opt3\|}` | [`SnippetToken::Choice`] |
//! | `$VAR` / `${VAR}` | [`SnippetToken::Variable`] |
//! | `${VAR:fallback}` | [`SnippetToken::Variable`] (with `default`) |
//! | `${1/pat/repl/flags}` / `${VAR/pat/repl/flags}` | [`SnippetToken::Transform`] |
//! | `\$`, `\\`, `\}` | unescaped → contributes to a [`SnippetToken::Literal`] |

use serde::{Deserialize, Serialize};

/// A parsed snippet body. Just a wrapper over the token vec so
/// the type signature in registry / load / render stays
/// readable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnippetBody {
    pub tokens: Vec<SnippetToken>,
}

impl SnippetBody {
    pub fn new(tokens: Vec<SnippetToken>) -> Self {
        Self { tokens }
    }

    pub fn iter(&self) -> std::slice::Iter<'_, SnippetToken> {
        self.tokens.iter()
    }

    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }
}

/// Choice placeholder option (`${1|opt1,opt2,opt3|}` -> three
/// options). Stored verbatim; the picker UI renders them as
/// alternatives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceOption {
    pub text: String,
}

/// One token from a parsed snippet body.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SnippetToken {
    /// Plain text that lands in the rendered output verbatim.
    /// Adjacent literal tokens are merged by the parser so a
    /// rendered body never has two literals in a row.
    Literal(String),
    /// `$N` -- bare tabstop. `$0` is the final cursor position
    /// (exited automatically); `$1..$N` are visited in order.
    Tabstop(u32),
    /// `${N:default}` -- tabstop with default text. The default
    /// can itself contain nested tokens (e.g. `${1:foo${2:bar}}`)
    /// so users can place inner placeholders inside.
    Placeholder { idx: u32, default: SnippetBody },
    /// `${N|opt1,opt2,opt3|}` -- pick from a fixed list. The
    /// host's choice-picker UI surfaces these on focus.
    Choice { idx: u32, options: Vec<ChoiceOption> },
    /// `$NAME` / `${NAME}` / `${NAME:fallback}` -- variable
    /// substitution. `default` runs only when the variable
    /// resolves to None.
    Variable {
        name: String,
        default: Option<SnippetBody>,
    },
    /// `${N/pat/repl/flags}` / `${NAME/pat/repl/flags}` --
    /// regex transformation on the bound text. v1 parses but
    /// renders as the bound text un-transformed; full regex
    /// support lands as polish (transformations are rare in
    /// practice, and supporting the full regex format syntax
    /// is its own feature).
    Transform {
        target: TransformTarget,
        pattern: String,
        replacement: String,
        flags: String,
    },
}

/// What a `Transform` token applies to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransformTarget {
    Tabstop(u32),
    Variable(String),
}
