//! Typed options registry (DESIGN.md §5.12).
//!
//! Every option is registered as a typed [`OptionSpec`] -- the name,
//! type, default, doc, and accessor functions live in one place.
//! `:set option=value` parses the value against the option's type;
//! `:set option` toggles a boolean or shows the current value;
//! `:set nooption` clears a boolean. `:describe-option name` opens
//! a help view with the spec's metadata.
//!
//! v1 status (B.2): a small but real set of options covering the
//! existing ad-hoc fields (number / relativenumber) plus new view
//! options (wrap, scrolloff, tabstop, ignorecase). Each spec
//! mutates [`App`] state through a small accessor closure pair so
//! no global state is involved -- adding an option is a matter of
//! adding a spec and (if needed) an App field.

use std::sync::Arc;

/// Lightweight typed value. `:set` parses the user's input against
/// the option's [`OptionKind`] and produces one of these. The
/// `setter` for the matching spec then applies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionValue {
    Bool(bool),
    Int(i64),
    String(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    Bool,
    Int,
    String,
}

impl OptionKind {
    pub fn label(self) -> &'static str {
        match self {
            OptionKind::Bool => "boolean",
            OptionKind::Int => "integer",
            OptionKind::String => "string",
        }
    }
}

/// Getter closure shape (read App state into an [`OptionValue`]).
type GetFn = Box<dyn Fn(&crate::app::App) -> OptionValue + Send + Sync>;

/// Setter closure shape (mutate App state from a typed
/// [`OptionValue`]). Returns an error string for invalid values
/// (out-of-range integers, type mismatches).
type SetFn = Box<dyn Fn(&mut crate::app::App, OptionValue) -> Result<(), String> + Send + Sync>;

/// One option's full metadata. Functionally a pair of accessor
/// closures plus the user-facing labels and default. The `Send +
/// Sync` bound on the closures lets specs live behind an `Arc` --
/// the App keeps a static `Vec<Arc<OptionSpec>>` instead of a
/// `Vec<OptionSpec>` so handlers don't need to clone the closure
/// state.
pub struct OptionSpec {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub doc: &'static str,
    pub kind: OptionKind,
    pub default: OptionValue,
    pub get: GetFn,
    pub set: SetFn,
}

impl std::fmt::Debug for OptionSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OptionSpec")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("default", &self.default)
            .field("aliases", &self.aliases)
            .finish_non_exhaustive()
    }
}

/// Registry holding every registered option. Built once at App
/// init via [`builtin_options`] and shared via `Arc<Self>` so the
/// `:` parser and the `:describe-option` view can both read it
/// without lifetime gymnastics.
#[derive(Debug, Default)]
pub struct OptionRegistry {
    specs: Vec<Arc<OptionSpec>>,
}

impl OptionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: OptionSpec) {
        self.specs.push(Arc::new(spec));
    }

    /// Find a spec by name OR alias. Case-sensitive (vim's `:set`
    /// is too).
    pub fn lookup(&self, name: &str) -> Option<Arc<OptionSpec>> {
        self.specs
            .iter()
            .find(|s| s.name == name || s.aliases.contains(&name))
            .cloned()
    }

    /// Find a spec for a `noNAME` form -- strips the `no` prefix
    /// and looks up the underlying boolean. Returns `None` if the
    /// stripped name doesn't resolve OR if the underlying option
    /// isn't a boolean (a non-boolean shouldn't accept `nofoo`).
    pub fn lookup_no_form(&self, name: &str) -> Option<Arc<OptionSpec>> {
        let stripped = name.strip_prefix("no")?;
        let spec = self.lookup(stripped)?;
        if matches!(spec.kind, OptionKind::Bool) {
            Some(spec)
        } else {
            None
        }
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.specs.iter().map(|s| s.name)
    }

    pub fn len(&self) -> usize {
        self.specs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.specs.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Arc<OptionSpec>> {
        self.specs.iter()
    }
}

/// Parse the body of `:set` into a structured request. Supports:
///
/// - `name` -- show value (booleans toggle on, others print).
/// - `name=value` -- set typed value.
/// - `noname` -- clear boolean.
///
/// Multiple options separated by whitespace are NOT yet handled --
/// vim's `:set ic hls scs` is deferred to a follow-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedSet {
    /// `:set name` -- query / boolean-on.
    NameOnly(String),
    /// `:set name=value` -- set typed value.
    Assign { name: String, value: String },
    /// `:set noname` -- clear boolean.
    Negate(String),
}

pub fn parse_set(input: &str) -> Result<ParsedSet, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty :set".into());
    }
    if let Some(eq) = trimmed.find('=') {
        let name = trimmed[..eq].trim().to_string();
        let value = trimmed[eq + 1..].trim().to_string();
        if name.is_empty() {
            return Err("empty option name".into());
        }
        return Ok(ParsedSet::Assign { name, value });
    }
    if let Some(rest) = trimmed.strip_prefix("no") {
        // Disambiguate `nopaste` (negate boolean `paste`) from a
        // hypothetical option literally named `noXXX`. v1: every
        // `no...` is treated as a negation; the registry's
        // `lookup_no_form` rejects non-boolean cases.
        return Ok(ParsedSet::Negate(rest.to_string()));
    }
    Ok(ParsedSet::NameOnly(trimmed.to_string()))
}

pub fn parse_value(value: &str, kind: OptionKind) -> Result<OptionValue, String> {
    match kind {
        OptionKind::Bool => match value {
            "true" | "1" | "on" | "yes" => Ok(OptionValue::Bool(true)),
            "false" | "0" | "off" | "no" | "" => Ok(OptionValue::Bool(false)),
            other => Err(format!(
                "expected boolean (true/false/on/off/1/0), got `{other}`"
            )),
        },
        OptionKind::Int => value
            .parse::<i64>()
            .map(OptionValue::Int)
            .map_err(|_| format!("expected integer, got `{value}`")),
        OptionKind::String => Ok(OptionValue::String(value.to_string())),
    }
}

/// Format an [`OptionValue`] for display in the echo area.
pub fn format_value(v: &OptionValue) -> String {
    match v {
        OptionValue::Bool(b) => b.to_string(),
        OptionValue::Int(i) => i.to_string(),
        OptionValue::String(s) => s.clone(),
    }
}

/// Build the v1 set of registered options. The actual mutation
/// closures bind App fields by name -- this is the single seam
/// connecting option metadata to App state. Adding a new option
/// means appending a spec here (and an App field if the option
/// has nowhere to live yet).
pub fn builtin_options() -> OptionRegistry {
    let mut r = OptionRegistry::new();
    r.register(OptionSpec {
        name: "number",
        aliases: &["nu"],
        doc: "Show absolute line numbers in the gutter.",
        kind: OptionKind::Bool,
        default: OptionValue::Bool(true),
        get: Box::new(|app| OptionValue::Bool(app.show_line_numbers)),
        set: Box::new(|app, v| match v {
            OptionValue::Bool(b) => {
                app.show_line_numbers = b;
                Ok(())
            }
            other => Err(format!("expected bool, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "relativenumber",
        aliases: &["rnu"],
        doc: "Gutter shows distance from the cursor; the cursor's line shows its absolute number.",
        kind: OptionKind::Bool,
        default: OptionValue::Bool(false),
        get: Box::new(|app| OptionValue::Bool(app.relative_line_numbers)),
        set: Box::new(|app, v| match v {
            OptionValue::Bool(b) => {
                app.relative_line_numbers = b;
                if b {
                    // Vim's behaviour: `:set rnu` implies number stays on.
                    app.show_line_numbers = true;
                }
                Ok(())
            }
            other => Err(format!("expected bool, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "wrap",
        aliases: &[],
        doc: "Wrap long lines visually instead of horizontal scrolling.",
        kind: OptionKind::Bool,
        default: OptionValue::Bool(false),
        get: Box::new(|app| OptionValue::Bool(app.wrap_lines)),
        set: Box::new(|app, v| match v {
            OptionValue::Bool(b) => {
                app.wrap_lines = b;
                Ok(())
            }
            other => Err(format!("expected bool, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "ignorecase",
        aliases: &["ic"],
        doc: "Ignore case in search patterns.",
        kind: OptionKind::Bool,
        default: OptionValue::Bool(false),
        get: Box::new(|app| OptionValue::Bool(app.ignorecase)),
        set: Box::new(|app, v| match v {
            OptionValue::Bool(b) => {
                app.ignorecase = b;
                Ok(())
            }
            other => Err(format!("expected bool, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "tabstop",
        aliases: &["ts"],
        doc: "Number of spaces a hard tab character renders as.",
        kind: OptionKind::Int,
        default: OptionValue::Int(8),
        get: Box::new(|app| OptionValue::Int(app.tabstop as i64)),
        set: Box::new(|app, v| match v {
            OptionValue::Int(i) if (1..=32).contains(&i) => {
                app.tabstop = i as u32;
                Ok(())
            }
            OptionValue::Int(i) => Err(format!("tabstop out of range [1, 32]: {i}")),
            other => Err(format!("expected int, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "foldmethod",
        aliases: &["fdm"],
        doc: "How folds are produced: `manual` (zf only), `indent` (auto from \
              indentation), `markdown` (ATX heading nesting), or `syntax` (tree-sitter \
              cascade -- markdown for `.md`, indent otherwise).",
        kind: OptionKind::String,
        default: OptionValue::String("manual".into()),
        get: Box::new(|app| OptionValue::String(app.foldmethod.label().into())),
        set: Box::new(|app, v| match v {
            OptionValue::String(s) => match s.as_str() {
                "manual" => {
                    app.foldmethod = crate::app::FoldMethod::Manual;
                    Ok(())
                }
                "indent" => {
                    app.foldmethod = crate::app::FoldMethod::Indent;
                    app.recompute_folds();
                    Ok(())
                }
                "markdown" => {
                    app.foldmethod = crate::app::FoldMethod::Markdown;
                    app.recompute_folds();
                    Ok(())
                }
                "syntax" => {
                    app.foldmethod = crate::app::FoldMethod::Syntax;
                    app.recompute_folds();
                    Ok(())
                }
                other => Err(format!(
                    "expected `manual`, `indent`, `markdown`, or `syntax`, got `{other}`"
                )),
            },
            other => Err(format!("expected string, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "ui.dim_inactive",
        aliases: &[],
        doc: "Apply a `DIM` overlay on inactive panes' syntax-highlighted text \
              so the active pane stands out without losing color.",
        kind: OptionKind::Bool,
        default: OptionValue::Bool(true),
        get: Box::new(|app| OptionValue::Bool(app.theme.dim_inactive_panes)),
        set: Box::new(|app, v| match v {
            OptionValue::Bool(b) => {
                app.theme.dim_inactive_panes = b;
                Ok(())
            }
            other => Err(format!("expected bool, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "ui.separator",
        aliases: &[],
        doc: "Character drawn in the column separating side-by-side panes (default `│`).",
        kind: OptionKind::String,
        default: OptionValue::String("│".into()),
        get: Box::new(|app| OptionValue::String(app.theme.pane_separator_vertical.to_string())),
        set: Box::new(|app, v| match v {
            OptionValue::String(s) => {
                let c = s
                    .chars()
                    .next()
                    .ok_or_else(|| "expected one character".to_string())?;
                app.theme.pane_separator_vertical = c;
                Ok(())
            }
            other => Err(format!("expected string, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "ui.separator_color",
        aliases: &[],
        doc: "Foreground color of the pane separator. Accepts named ANSI colors \
              (red, blue, darkgray, ...) and `default` for the terminal default.",
        kind: OptionKind::String,
        default: OptionValue::String("darkgray".into()),
        get: Box::new(|_| OptionValue::String("darkgray".into())),
        set: Box::new(|app, v| match v {
            OptionValue::String(s) => {
                let c = crate::theme::parse_color(&s)?;
                app.theme.pane_separator = ratatui::style::Style::default().fg(c);
                Ok(())
            }
            other => Err(format!("expected string, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "ui.statusline_active_fg",
        aliases: &[],
        doc: "Foreground color of the active pane's status line.",
        kind: OptionKind::String,
        default: OptionValue::String("default".into()),
        get: Box::new(|_| OptionValue::String("default".into())),
        set: Box::new(|app, v| match v {
            OptionValue::String(s) => {
                let c = crate::theme::parse_color(&s)?;
                app.theme.pane_status_active = app.theme.pane_status_active.fg(c);
                Ok(())
            }
            other => Err(format!("expected string, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "ui.statusline_inactive_fg",
        aliases: &[],
        doc: "Foreground color of inactive panes' status lines.",
        kind: OptionKind::String,
        default: OptionValue::String("darkgray".into()),
        get: Box::new(|_| OptionValue::String("darkgray".into())),
        set: Box::new(|app, v| match v {
            OptionValue::String(s) => {
                let c = crate::theme::parse_color(&s)?;
                app.theme.pane_status_inactive = app.theme.pane_status_inactive.fg(c);
                Ok(())
            }
            other => Err(format!("expected string, got {other:?}")),
        }),
    });
    r.register(OptionSpec {
        name: "scrolloff",
        aliases: &["so"],
        doc: "Minimum visual lines kept above and below the cursor when scrolling.",
        kind: OptionKind::Int,
        default: OptionValue::Int(0),
        get: Box::new(|app| OptionValue::Int(app.scrolloff as i64)),
        set: Box::new(|app, v| match v {
            OptionValue::Int(i) if (0..=64).contains(&i) => {
                app.scrolloff = i as u32;
                Ok(())
            }
            OptionValue::Int(i) => Err(format!("scrolloff out of range [0, 64]: {i}")),
            other => Err(format!("expected int, got {other:?}")),
        }),
    });
    r
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn parse_set_name_only() {
        assert_eq!(
            parse_set("number").unwrap(),
            ParsedSet::NameOnly("number".into())
        );
    }

    #[test]
    fn parse_set_assign() {
        assert_eq!(
            parse_set("tabstop=4").unwrap(),
            ParsedSet::Assign {
                name: "tabstop".into(),
                value: "4".into()
            }
        );
    }

    #[test]
    fn parse_set_negate() {
        assert_eq!(
            parse_set("nonumber").unwrap(),
            ParsedSet::Negate("number".into())
        );
    }

    #[test]
    fn parse_set_empty_errors() {
        assert!(parse_set("   ").is_err());
    }

    #[test]
    fn parse_value_bool_accepts_common_truthy_falsy() {
        assert_eq!(
            parse_value("true", OptionKind::Bool).unwrap(),
            OptionValue::Bool(true)
        );
        assert_eq!(
            parse_value("on", OptionKind::Bool).unwrap(),
            OptionValue::Bool(true)
        );
        assert_eq!(
            parse_value("yes", OptionKind::Bool).unwrap(),
            OptionValue::Bool(true)
        );
        assert_eq!(
            parse_value("1", OptionKind::Bool).unwrap(),
            OptionValue::Bool(true)
        );
        assert_eq!(
            parse_value("false", OptionKind::Bool).unwrap(),
            OptionValue::Bool(false)
        );
        assert_eq!(
            parse_value("off", OptionKind::Bool).unwrap(),
            OptionValue::Bool(false)
        );
        assert_eq!(
            parse_value("no", OptionKind::Bool).unwrap(),
            OptionValue::Bool(false)
        );
        assert_eq!(
            parse_value("0", OptionKind::Bool).unwrap(),
            OptionValue::Bool(false)
        );
    }

    #[test]
    fn parse_value_int_round_trips() {
        assert_eq!(
            parse_value("4", OptionKind::Int).unwrap(),
            OptionValue::Int(4)
        );
        assert!(parse_value("nope", OptionKind::Int).is_err());
    }

    #[test]
    fn registry_lookup_resolves_alias() {
        let r = builtin_options();
        let by_alias = r.lookup("nu").unwrap();
        assert_eq!(by_alias.name, "number");
    }

    #[test]
    fn registry_lookup_no_form_only_for_booleans() {
        let r = builtin_options();
        // `nonumber` resolves to `number` (boolean).
        assert!(r.lookup_no_form("nonumber").is_some());
        // `notabstop` -- tabstop is int, not boolean -> rejected.
        assert!(r.lookup_no_form("notabstop").is_none());
    }
}
