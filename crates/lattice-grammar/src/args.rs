//! Per-command argument values.
//!
//! Each registered command declares an `args_schema: Vec<ArgSpec>` (DESIGN.md
//! §5.11, §B.1) describing the kinds, names, prompts and defaults for its
//! arguments. The dispatcher carries the concrete values through `Args`.
//!
//! Three Args shapes coexist:
//!
//! - `Args::None` -- universal "no args" form (most motions / operators).
//! - `Args::Char(char)` / `Args::String(String)` -- single-arg shortcuts
//!   for the common cases (vim's `f<x>` takes a char; `:set <opt>` takes a
//!   string). Predates B.1 and stays for ergonomic registration.
//! - `Args::List(Vec<ArgValue>)` -- multi-arg form, positional values
//!   matching the command's `args_schema` in declaration order. This is
//!   what palette-driven entry, plugin invocations, and the `:`-line
//!   parser front-end produce for ex-commands with structured args
//!   (`:s/pat/repl/flags`, `:g/pat/body`).
//! - `Args::Bytes(Vec<u8>)` -- escape hatch for plugin-supplied richer
//!   args, encoded msgpack-style on the wire. When WASM lands, WIT-typed
//!   args replace this byte form.
//!
//! `ArgValue` is a small typed enum -- not a dynamic value bag -- so callers
//! get static type checks at the boundary.

use serde::{Deserialize, Serialize};

use crate::command::CommandInvocation;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
pub enum Args {
    #[default]
    None,
    Char(char),
    String(String),
    Bytes(Vec<u8>),
    /// Multi-arg form. Values appear in the order declared by the
    /// command's `args_schema`.
    List(Vec<ArgValue>),
}

impl Args {
    pub fn is_none(&self) -> bool {
        matches!(self, Args::None)
    }

    /// Borrow the multi-arg list, if present. Returns `None` for any
    /// other variant.
    pub fn as_list(&self) -> Option<&[ArgValue]> {
        match self {
            Args::List(v) => Some(v),
            _ => None,
        }
    }
}

/// One argument's typed value. The variants mirror [`ArgKind`] one-for-one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ArgValue {
    String(String),
    Char(char),
    Bool(bool),
    Int(i64),
    /// A pattern -- regex when v1 grows regex; literal substring today.
    Pattern(String),
    /// A keyboard chord in canonical notation (`<C-c>`, `<Esc>`, `gg`,
    /// `<C-S-x>`). Stored as a string so the value layer doesn't need
    /// to know about crossterm / KeyEvent; the UI captures raw key
    /// events and renders them via `format_chord` before the value
    /// reaches here. Used by `:describe-key` and (later) `:map`,
    /// `:nnoremap`, etc.
    Chord(String),
    /// A nested invocation. Used by `:g/.../body` where the body is a
    /// command in its own right; the parser front-end produces a
    /// `CommandInvocation` and the host dispatches it per match. Boxed
    /// because Args lives inside CommandInvocation.
    Invocation(Box<CommandInvocation>),
    /// Raw text whose parsing was deferred. v1 uses this for `:g`'s
    /// body string (re-parsed per line by the host) until we promote
    /// body to a parsed `Invocation`.
    Raw(String),
}

impl ArgValue {
    pub fn kind(&self) -> ArgKind {
        match self {
            ArgValue::String(_) => ArgKind::String,
            ArgValue::Char(_) => ArgKind::Char,
            ArgValue::Bool(_) => ArgKind::Bool,
            ArgValue::Int(_) => ArgKind::Int,
            ArgValue::Pattern(_) => ArgKind::Pattern,
            ArgValue::Chord(_) => ArgKind::Chord,
            ArgValue::Invocation(_) => ArgKind::Body,
            ArgValue::Raw(_) => ArgKind::Raw,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ArgValue::String(s)
            | ArgValue::Pattern(s)
            | ArgValue::Chord(s)
            | ArgValue::Raw(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ArgValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Extract a parsed sub-invocation. Used by `:g`'s body slot to
    /// pull the pre-parsed `CommandInvocation` out at apply time.
    pub fn as_invocation(&self) -> Option<&CommandInvocation> {
        match self {
            ArgValue::Invocation(inv) => Some(inv.as_ref()),
            _ => None,
        }
    }
}

/// Type tag for an argument. Mirrors [`ArgValue`] one-for-one and is
/// what an `args_schema` entry declares for each positional argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArgKind {
    String,
    Char,
    Bool,
    Int,
    Pattern,
    /// A keyboard chord. UI surfaces switch the cmdline into
    /// chord-capture mode while the cursor sits in this slot --
    /// raw key events are translated to canonical chord notation
    /// (`<C-c>`, `<Esc>`, ...) and inserted as one token.
    Chord,
    /// A parsed sub-invocation (a CommandInvocation in its own right).
    Body,
    /// Unparsed text (host re-parses or interprets later).
    Raw,
}

/// What the runtime should fall back to when an arg is unsupplied at
/// invocation time (DESIGN.md §B.1). For interactive entry, the fallback
/// chain is: caller-supplied value -> `default` -> prompt the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ArgDefault {
    /// No fallback -- the runtime prompts (or errors, in non-interactive
    /// callers) when this arg is missing.
    Required,
    /// Optional. Missing means "absent"; the apply closure must handle
    /// the absence gracefully.
    None,
    /// A literal default value.
    Literal(ArgValue),
    /// Use the current visual selection's text. Useful for `:s` invoked
    /// over a visual range: the default pattern is "the selected text".
    UseSelection,
    /// Use the word under the cursor. Useful for `:grep`-style commands.
    UseCursorWord,
    /// Use the most recently entered value of this argument. Useful for
    /// `:s` to default to the previous pattern + replacement.
    UseLastResponse,
}

/// One argument's metadata. A command's `args_schema` is the ordered list
/// of these. Drives:
///
/// 1. The `:` parser front-end's structured-args extraction (when the
///    syntax permits; delimiter-syntax commands still own their own
///    `parse_args`).
/// 2. Keymap binding pre-supply: a binding may set some args ahead of
///    time and prompt for the rest.
/// 3. Command palette / interactive form: each missing arg becomes a
///    prompt with the schema-supplied prompt text + completion.
/// 4. `:describe-command` enumeration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArgSpec {
    /// Identifier shown in `:describe-command` output and used as the
    /// minibuffer prompt label.
    pub name: &'static str,
    pub kind: ArgKind,
    /// One-line documentation. Surfaced in palette tooltips and
    /// `:describe-command`.
    pub doc: &'static str,
    /// Prompt shown when the runtime needs to ask for this arg
    /// interactively. Empty string means "use `name` as the prompt".
    pub prompt: &'static str,
    pub default: ArgDefault,
    /// Name of the registered completion source (`gen:commands`,
    /// `gen:files`, etc. -- see `lattice-completion`) that fires
    /// when the user is typing this arg. `None` = no completion
    /// (free-form text). Wire-form is the source name (not its
    /// runtime id) so the schema is constructable as a literal.
    pub completion: Option<&'static str>,
}

impl ArgSpec {
    /// Sugar for declaring a required arg with no fancy default.
    pub fn required(name: &'static str, kind: ArgKind, doc: &'static str) -> Self {
        Self {
            name,
            kind,
            doc,
            prompt: "",
            default: ArgDefault::Required,
            completion: None,
        }
    }

    /// Sugar for declaring an optional arg.
    pub fn optional(name: &'static str, kind: ArgKind, doc: &'static str) -> Self {
        Self {
            name,
            kind,
            doc,
            prompt: "",
            default: ArgDefault::None,
            completion: None,
        }
    }

    /// Builder helper: attach a completion source by registered name.
    pub fn with_completion(mut self, source_name: &'static str) -> Self {
        self.completion = Some(source_name);
        self
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn default_is_none() {
        assert_eq!(Args::default(), Args::None);
        assert!(Args::None.is_none());
    }

    #[test]
    fn char_carries_character() {
        let a = Args::Char('x');
        assert!(!a.is_none());
        match a {
            Args::Char(c) => assert_eq!(c, 'x'),
            _ => panic!("expected Char"),
        }
    }

    #[test]
    fn string_args_round_trip() {
        let a = Args::String("hello".into());
        let json = serde_json::to_string(&a).unwrap();
        let back: Args = serde_json::from_str(&json).unwrap();
        assert_eq!(back, a);
    }

    #[test]
    fn list_args_carry_positional_values() {
        let a = Args::List(vec![
            ArgValue::Pattern("foo".into()),
            ArgValue::String("bar".into()),
            ArgValue::Bool(true),
        ]);
        let list = a.as_list().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].kind(), ArgKind::Pattern);
        assert_eq!(list[2].as_bool(), Some(true));
    }

    #[test]
    fn arg_value_kind_matches_variant() {
        assert_eq!(ArgValue::String("x".into()).kind(), ArgKind::String);
        assert_eq!(ArgValue::Char('x').kind(), ArgKind::Char);
        assert_eq!(ArgValue::Bool(false).kind(), ArgKind::Bool);
        assert_eq!(ArgValue::Int(42).kind(), ArgKind::Int);
        assert_eq!(ArgValue::Pattern("p".into()).kind(), ArgKind::Pattern);
        assert_eq!(ArgValue::Raw("r".into()).kind(), ArgKind::Raw);
    }

    #[test]
    fn arg_value_as_str_covers_string_variants() {
        assert_eq!(ArgValue::String("a".into()).as_str(), Some("a"));
        assert_eq!(ArgValue::Pattern("b".into()).as_str(), Some("b"));
        assert_eq!(ArgValue::Raw("c".into()).as_str(), Some("c"));
        assert_eq!(ArgValue::Bool(true).as_str(), None);
    }

    #[test]
    fn arg_value_round_trips_through_json() {
        let v = ArgValue::Pattern("hello".into());
        let s = serde_json::to_string(&v).unwrap();
        let back: ArgValue = serde_json::from_str(&s).unwrap();
        assert_eq!(back, v);
    }

    #[test]
    fn arg_spec_required_sugar() {
        let s = ArgSpec::required("path", ArgKind::String, "file path");
        assert_eq!(s.name, "path");
        assert_eq!(s.kind, ArgKind::String);
        assert!(matches!(s.default, ArgDefault::Required));
    }

    #[test]
    fn arg_spec_optional_sugar() {
        let s = ArgSpec::optional("flag", ArgKind::Bool, "doc");
        assert!(matches!(s.default, ArgDefault::None));
    }

    #[test]
    fn args_list_serde_round_trip() {
        let a = Args::List(vec![
            ArgValue::Pattern("p".into()),
            ArgValue::String("r".into()),
            ArgValue::Bool(true),
        ]);
        let s = serde_json::to_string(&a).unwrap();
        let back: Args = serde_json::from_str(&s).unwrap();
        assert_eq!(back, a);
    }
}
