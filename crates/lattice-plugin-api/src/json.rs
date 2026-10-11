//! AD.2: the catalog as JSON — the plugin API for a reader that wants
//! structure rather than prose (an agent, a code generator, an editor
//! integration).
//!
//! Hand-written rather than `serde`: this crate's contract is ZERO runtime
//! dependencies (`lattice-host` links it, and whatever it depends on the host
//! inherits), and the shape is small and fixed. Output is pretty-printed with
//! stable key order so the checked-in file diffs line by line when the WIT
//! moves.
//!
//! Shape (every field that can be absent is `null`, never omitted, so a
//! consumer can rely on the keys):
//!
//! ```text
//! { "package": "lattice:plugin-host@0.2.0",
//!   "interfaces": [ { "name", "doc", "direction", "capability",
//!       "examples": [ EXAMPLE ],
//!       "functions": [ { "name", "display_name", "kind", "resource",
//!                        "async", "params": [ {"name","type"} ],
//!                        "result", "signature", "doc",
//!                        "examples": [ EXAMPLE ] } ],
//!       "types": [ { "name", "kind", "doc", "definition",
//!                    "members": [ {"name","type","doc"} ],
//!                    "examples": [ EXAMPLE ] } ],
//!       "uses": [ { "name", "from", "original" } ] } ],
//!   "worlds": [ { "name", "doc", "imports", "exports",
//!                 "export_functions": [ FUNCTION ],
//!                 "import_functions": [ FUNCTION ] } ] }
//!
//! FUNCTION = { "name", "display_name", "kind", "resource", "async",
//!              "params", "result", "signature", "doc", "examples" }
//!
//! EXAMPLE = { "id", "caption", "source", "language", "code" }
//! ```
//!
//! Examples sit on the item they illustrate (a method's on the method, a
//! resource-level one on the resource's type entry), so a consumer reading a
//! function has its examples without a second lookup.

use crate::examples::Examples;
use crate::render::{capability_short, direction_short, wit_definition};
use crate::{ApiFunctionKind, ApiTypeKind, PluginApiCatalog};

/// The whole catalog as pretty-printed JSON, newline-terminated. `examples`
/// is the guest scan, or `None` to emit empty `examples` arrays.
pub fn to_json(cat: &PluginApiCatalog, examples: Option<&Examples>) -> String {
    let interfaces = cat
        .interfaces
        .iter()
        .map(|i| {
            let examples_for = |item: Option<&str>| {
                let target = match item {
                    Some(item) => format!("{}.{item}", i.name),
                    None => i.name.clone(),
                };
                Json::Arr(
                    examples
                        .map(|ex| {
                            ex.for_target(&target)
                                .into_iter()
                                .map(|e| {
                                    Json::obj([
                                        ("id", s(&e.id)),
                                        ("caption", s(&e.caption)),
                                        ("source", s(&e.source)),
                                        ("language", s("rust")),
                                        ("code", s(&e.code)),
                                    ])
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                )
            };
            let functions = i
                .functions
                .iter()
                .map(|f| function_json(f, examples_for(Some(&f.display_name()))))
                .collect();
            let types = i
                .types
                .iter()
                .map(|t| {
                    let members = t
                        .kind
                        .members()
                        .iter()
                        .map(|m| {
                            Json::obj([
                                ("name", s(&m.name)),
                                ("type", opt(m.ty.as_deref())),
                                ("doc", opt(m.doc.as_deref())),
                            ])
                        })
                        .collect();
                    let kind = match t.kind {
                        ApiTypeKind::Alias(_) => "alias",
                        _ => t.kind.keyword(),
                    };
                    Json::obj([
                        ("name", s(&t.name)),
                        ("kind", s(kind)),
                        ("doc", opt(t.doc.as_deref())),
                        ("definition", s(&wit_definition(t))),
                        ("members", Json::Arr(members)),
                        ("examples", examples_for(Some(&t.name))),
                    ])
                })
                .collect();
            let uses = i
                .uses
                .iter()
                .map(|u| {
                    Json::obj([
                        ("name", s(&u.name)),
                        ("from", s(&u.from)),
                        ("original", s(&u.original)),
                    ])
                })
                .collect();
            Json::obj([
                ("name", s(&i.name)),
                ("doc", opt(i.doc.as_deref())),
                ("direction", s(direction_short(i.direction))),
                ("capability", s(capability_json(i.capability))),
                ("examples", examples_for(None)),
                ("functions", Json::Arr(functions)),
                ("types", Json::Arr(types)),
                ("uses", Json::Arr(uses)),
            ])
        })
        .collect();
    let worlds = cat
        .worlds
        .iter()
        .map(|w| {
            Json::obj([
                ("name", s(&w.name)),
                ("doc", opt(w.doc.as_deref())),
                (
                    "imports",
                    Json::Arr(w.imports.iter().map(|n| s(n)).collect()),
                ),
                (
                    "exports",
                    Json::Arr(w.exports.iter().map(|n| s(n)).collect()),
                ),
                (
                    "export_functions",
                    Json::Arr(
                        w.export_functions
                            .iter()
                            .map(|f| function_json(f, Json::Arr(Vec::new())))
                            .collect(),
                    ),
                ),
                (
                    "import_functions",
                    Json::Arr(
                        w.import_functions
                            .iter()
                            .map(|f| function_json(f, Json::Arr(Vec::new())))
                            .collect(),
                    ),
                ),
            ])
        })
        .collect();
    let root = Json::obj([
        ("package", s(crate::PACKAGE)),
        ("interfaces", Json::Arr(interfaces)),
        ("worlds", Json::Arr(worlds)),
    ]);
    let mut out = String::new();
    root.write(&mut out, 0);
    out.push('\n');
    out
}

/// One function as JSON; `examples` is the already-built array.
fn function_json(f: &crate::ApiFunction, examples: Json) -> Json {
    let (kind, resource) = match &f.kind {
        ApiFunctionKind::Freestanding => ("freestanding", None),
        ApiFunctionKind::Method(r) => ("method", Some(r.as_str())),
        ApiFunctionKind::Static(r) => ("static", Some(r.as_str())),
        ApiFunctionKind::Constructor(r) => ("constructor", Some(r.as_str())),
    };
    let params = f
        .params
        .iter()
        .map(|p| Json::obj([("name", s(&p.name)), ("type", s(&p.ty))]))
        .collect();
    Json::obj([
        ("name", s(&f.name)),
        ("display_name", s(&f.display_name())),
        ("kind", s(kind)),
        ("resource", opt(resource)),
        ("async", Json::Bool(f.is_async)),
        ("params", Json::Arr(params)),
        ("result", opt(f.result.as_deref())),
        ("signature", s(&f.signature())),
        ("doc", opt(f.doc.as_deref())),
        ("examples", examples),
    ])
}

/// `none` rather than `-`: a JSON consumer should not have to know the
/// table-cell convention of the markdown renderer.
fn capability_json(c: crate::Capability) -> &'static str {
    match capability_short(c) {
        "-" => "none",
        other => other,
    }
}

/// The minimal JSON value this module needs.
enum Json {
    Str(String),
    Bool(bool),
    Null,
    Arr(Vec<Json>),
    /// Key order is insertion order — the order the shape above documents.
    Obj(Vec<(&'static str, Json)>),
}

fn s(v: &str) -> Json {
    Json::Str(v.to_string())
}

fn opt(v: Option<&str>) -> Json {
    v.map(s).unwrap_or(Json::Null)
}

impl Json {
    fn obj<const N: usize>(fields: [(&'static str, Json); N]) -> Json {
        Json::Obj(fields.into_iter().collect())
    }

    fn write(&self, out: &mut String, depth: usize) {
        let pad = |d: usize| "  ".repeat(d);
        match self {
            Json::Str(v) => write_str(out, v),
            Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            Json::Null => out.push_str("null"),
            Json::Arr(items) if items.is_empty() => out.push_str("[]"),
            Json::Arr(items) => {
                out.push_str("[\n");
                for (n, item) in items.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    item.write(out, depth + 1);
                    out.push_str(if n + 1 < items.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push(']');
            }
            Json::Obj(fields) => {
                out.push_str("{\n");
                for (n, (key, value)) in fields.iter().enumerate() {
                    out.push_str(&pad(depth + 1));
                    write_str(out, key);
                    out.push_str(": ");
                    value.write(out, depth + 1);
                    out.push_str(if n + 1 < fields.len() { ",\n" } else { "\n" });
                }
                out.push_str(&pad(depth));
                out.push('}');
            }
        }
    }
}

/// A JSON string literal: quotes, backslashes and control characters escaped
/// (RFC 8259 §7); everything else, including non-ASCII, passes through as
/// UTF-8.
fn write_str(out: &mut String, v: &str) {
    out.push('"');
    for c in v.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_escaped_per_rfc_8259() {
        let mut out = String::new();
        write_str(&mut out, "a\"b\\c\nd\u{1}é");
        assert_eq!(out, r#""a\"b\\c\nd\u0001é""#);
    }

    #[test]
    fn nesting_is_indented_and_empty_arrays_are_inline() {
        let mut out = String::new();
        Json::obj([("k", Json::Arr(vec![])), ("v", Json::Null)]).write(&mut out, 0);
        assert_eq!(out, "{\n  \"k\": [],\n  \"v\": null\n}");
    }
}
