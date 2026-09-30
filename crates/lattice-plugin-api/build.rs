//! PI.1 build script: derive the plugin-API catalog from the canonical `wit/`
//! package at build time.
//!
//! The `wit/` package IS the plugin API (plugin-host.md §5). Parsing it here —
//! rather than hand-authoring a catalog — means the catalog can never drift
//! from the WIT: a new interface / function / doc-comment shows up in
//! `:describe-plugin-api` (PI.2) the moment the WIT lands, with no second edit.
//!
//! Output: `$OUT_DIR/catalog.rs`, textually `include!`d by `src/lib.rs`. It
//! defines two free functions — `generated_interfaces()` and
//! `generated_worlds()` — returning the parsed data as the lib's public types
//! (`ApiInterface` / `ApiWorld`). Since AD.1 an interface carries its full
//! surface: function signatures (params, result, resource ownership), the
//! types it defines with every field / case, and the types it `use`s. WIT
//! type expressions are emitted as their WIT spelling (`type_str`), not
//! re-modelled — a reader and a renderer need the spelling, nothing more. The host-authored capability annotation is
//! merged in `lib.rs::catalog()`, not here (the WIT can't carry it, and this
//! script must stay capability-agnostic so the annotation has ONE home).
//!
//! Graceful error (four-artefact clause): a totally unparseable canonical WIT
//! is a hard build error — you cannot ship a plugin editor whose API package
//! is broken, and silently emitting an empty catalog would hide it. A merely
//! *odd* interface (no name — an inline world interface) is skipped with a
//! `cargo:warning`, never a panic.

// A build script's failure mechanism IS a panic (it aborts the build with the
// message), and `writeln!` into a `String` is infallible-by-construction — both
// `unwrap`/`panic` are correct here, so opt out of the workspace's (advisory)
// hot-path lints for this compile-host-only script.
#![allow(clippy::unwrap_used, clippy::panic)]

use std::collections::BTreeSet;
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use wit_parser::{
    FunctionKind, Handle, InterfaceId, Resolve, Type, TypeDefKind, TypeOwner, WorldItem,
};

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    // The canonical WIT package lives at the workspace root `wit/`.
    let wit_dir = manifest_dir.join("../lattice-wit/wit");

    // Regenerate whenever any `.wit` file (or the directory listing) changes.
    println!("cargo:rerun-if-changed={}", wit_dir.display());
    if let Ok(entries) = fs::read_dir(&wit_dir) {
        for entry in entries.flatten() {
            println!("cargo:rerun-if-changed={}", entry.path().display());
        }
    }

    let generated = parse_catalog(&wit_dir);

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("catalog.rs"), generated).expect("write catalog.rs");
}

/// World-derived direction of an interface, relative to a guest plugin.
///
/// `use foo.{ty}` registers an import edge indistinguishable from a callable
/// `import foo;`, so a pure type bag (`types`, zero functions) is imported yet
/// not *called* — only an imported interface that HAS functions is a genuine
/// guest→host call seam (`GuestImport`); a zero-function one is `TypesOnly`.
fn direction_literal(exported: bool, imported: bool, has_functions: bool) -> &'static str {
    let import_callable = imported && has_functions;
    match (exported, import_callable) {
        (true, true) => "Direction::Both",
        (true, false) => "Direction::GuestExport",
        (false, true) => "Direction::GuestImport",
        (false, false) => "Direction::TypesOnly",
    }
}

fn parse_catalog(wit_dir: &Path) -> String {
    let mut resolve = Resolve::default();
    // A totally unparseable canonical API is a hard build error (see module
    // doc). `push_dir` parses the flat single-package `wit/` directory.
    let (pkg_id, _sources) = resolve.push_dir(wit_dir).unwrap_or_else(|e| {
        panic!("lattice-plugin-api: failed to parse canonical wit/ package: {e:#}")
    });
    let package = &resolve.packages[pkg_id];

    // Which interfaces does any world export / import (as a function namespace)?
    // `WorldItem::Interface` is a callable-namespace edge; `use foo.{ty}` for
    // types alone also surfaces here, so direction is a descriptive hint, not a
    // contract — PI.2 renders it as such.
    let mut exported: BTreeSet<_> = BTreeSet::new();
    let mut imported: BTreeSet<_> = BTreeSet::new();
    for (_world_name, &world_id) in &package.worlds {
        let world = &resolve.worlds[world_id];
        for item in world.exports.values() {
            if let WorldItem::Interface { id, .. } = item {
                exported.insert(*id);
            }
        }
        for item in world.imports.values() {
            if let WorldItem::Interface { id, .. } = item {
                imported.insert(*id);
            }
        }
    }

    // --- interfaces (sorted by name for a deterministic catalog) ---
    let mut ifaces: Vec<_> = package
        .interfaces
        .iter()
        .filter_map(|(name, &id)| {
            let iface = &resolve.interfaces[id];
            // An interface with no name is an inline world interface, not a
            // declared API seam — skip it (odd, not fatal).
            match &iface.name {
                Some(n) => Some((n.clone(), id, iface)),
                None => {
                    println!(
                        "cargo:warning=lattice-plugin-api: skipping unnamed interface `{name}`"
                    );
                    None
                }
            }
        })
        .collect();
    ifaces.sort_by(|a, b| a.0.cmp(&b.0));

    let mut out = String::new();
    out.push_str("// @generated by build.rs from wit/ — do not edit.\n");
    out.push_str("fn generated_interfaces() -> Vec<ApiInterface> {\n\tvec![\n");
    for (name, id, iface) in &ifaces {
        let mut funcs: Vec<_> = iface.functions.values().collect();
        funcs.sort_by(|a, b| a.name.cmp(&b.name));
        let dir = direction_literal(
            exported.contains(id),
            imported.contains(id),
            !funcs.is_empty(),
        );
        writeln!(out, "\t\tApiInterface {{").unwrap();
        writeln!(out, "\t\t\tname: {}.to_string(),", lit(name)).unwrap();
        writeln!(
            out,
            "\t\t\tdoc: {},",
            opt_lit(iface.docs.contents.as_deref())
        )
        .unwrap();
        writeln!(out, "\t\t\tdirection: {dir},").unwrap();
        writeln!(
            out,
            "\t\t\t// capability is merged from CAPABILITY_ANNOTATIONS in catalog()."
        )
        .unwrap();
        writeln!(out, "\t\t\tcapability: Capability::None,").unwrap();
        writeln!(out, "\t\t\tfunctions: vec![").unwrap();
        for f in funcs {
            let (kind, is_async) = function_kind(&resolve, &f.kind);
            let params = f
                .params
                .iter()
                .map(|p| {
                    format!(
                        "ApiParam {{ name: {}.to_string(), ty: {}.to_string() }}",
                        lit(&p.name),
                        lit(&type_str(&resolve, &p.ty))
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            writeln!(
                out,
                "\t\t\t\tApiFunction {{ name: {}.to_string(), doc: {}, kind: {kind}, \
                 is_async: {is_async}, params: vec![{params}], result: {} }},",
                lit(&f.name),
                opt_lit(f.docs.contents.as_deref()),
                opt_lit(f.result.as_ref().map(|t| type_str(&resolve, t)).as_deref()),
            )
            .unwrap();
        }
        writeln!(out, "\t\t\t],").unwrap();

        // Types in WIT SOURCE order, not sorted: authors order a seam's types
        // so each one is read after what it depends on, and the reference is
        // read top to bottom. A `use` is an alias whose target another
        // interface owns — listed separately so a page links to the
        // definition instead of repeating it.
        let mut types = String::new();
        let mut uses = String::new();
        for (ty_name, &ty_id) in &iface.types {
            let def = &resolve.types[ty_id];
            if let TypeDefKind::Type(Type::Id(target)) = def.kind {
                let target_def = &resolve.types[target];
                if let TypeOwner::Interface(owner) = target_def.owner
                    && owner != *id
                {
                    writeln!(
                        uses,
                        "\t\t\t\tApiUse {{ name: {}.to_string(), from: {}.to_string(), \
                         original: {}.to_string() }},",
                        lit(ty_name),
                        lit(&interface_name(&resolve, owner)),
                        lit(target_def.name.as_deref().unwrap_or(ty_name)),
                    )
                    .unwrap();
                    continue;
                }
            }
            writeln!(
                types,
                "\t\t\t\tApiType {{ name: {}.to_string(), doc: {}, kind: {} }},",
                lit(ty_name),
                opt_lit(def.docs.contents.as_deref()),
                type_kind(&resolve, &def.kind),
            )
            .unwrap();
        }
        writeln!(out, "\t\t\ttypes: vec![\n{types}\t\t\t],").unwrap();
        writeln!(out, "\t\t\tuses: vec![\n{uses}\t\t\t],").unwrap();
        writeln!(out, "\t\t}},").unwrap();
    }
    out.push_str("\t]\n}\n\n");

    // --- worlds (sorted; the test-only trampoline fixture is not an API) ---
    let mut worlds: Vec<_> = package
        .worlds
        .iter()
        .filter(|(name, _)| name.as_str() != "trampoline-fixture")
        .map(|(name, &id)| (name.clone(), &resolve.worlds[id]))
        .collect();
    worlds.sort_by(|a, b| a.0.cmp(&b.0));

    out.push_str("fn generated_worlds() -> Vec<ApiWorld> {\n\tvec![\n");
    for (name, world) in &worlds {
        let mut imports: Vec<String> = world
            .imports
            .iter()
            .filter_map(|(_, item)| match item {
                WorldItem::Interface { id, .. } => resolve.interfaces[*id].name.clone(),
                _ => None,
            })
            .collect();
        let mut exports: Vec<String> = world
            .exports
            .iter()
            .filter_map(|(_, item)| match item {
                WorldItem::Interface { id, .. } => resolve.interfaces[*id].name.clone(),
                _ => None,
            })
            .collect();
        imports.sort();
        imports.dedup();
        exports.sort();
        exports.dedup();
        writeln!(out, "\t\tApiWorld {{").unwrap();
        writeln!(out, "\t\t\tname: {}.to_string(),", lit(name)).unwrap();
        writeln!(
            out,
            "\t\t\tdoc: {},",
            opt_lit(world.docs.contents.as_deref())
        )
        .unwrap();
        writeln!(out, "\t\t\timports: vec![{}],", str_vec(&imports)).unwrap();
        writeln!(out, "\t\t\texports: vec![{}],", str_vec(&exports)).unwrap();
        writeln!(out, "\t\t}},").unwrap();
    }
    out.push_str("\t]\n}\n");

    out
}

/// The name an interface is referred to by. Every seam lives in the one
/// `lattice:plugin-host` package, so the bare name is unambiguous.
fn interface_name(resolve: &Resolve, id: InterfaceId) -> String {
    resolve.interfaces[id]
        .name
        .clone()
        .unwrap_or_else(|| "<anonymous>".to_string())
}

/// A WIT type written back as WIT source: `list<option<string>>`,
/// `result<_, string>`, `borrow<document>`. A named type is written by its
/// name — the reader follows the link to its definition, exactly as they
/// would in the `.wit` file.
fn type_str(resolve: &Resolve, ty: &Type) -> String {
    match ty {
        Type::Bool => "bool".into(),
        Type::U8 => "u8".into(),
        Type::U16 => "u16".into(),
        Type::U32 => "u32".into(),
        Type::U64 => "u64".into(),
        Type::S8 => "s8".into(),
        Type::S16 => "s16".into(),
        Type::S32 => "s32".into(),
        Type::S64 => "s64".into(),
        Type::F32 => "f32".into(),
        Type::F64 => "f64".into(),
        Type::Char => "char".into(),
        Type::String => "string".into(),
        Type::ErrorContext => "error-context".into(),
        Type::Id(id) => {
            let def = &resolve.types[*id];
            if let Some(name) = &def.name {
                return name.clone();
            }
            anonymous_type_str(resolve, &def.kind)
        }
    }
}

/// The WIT spelling of an unnamed type expression (`list<T>`, `option<T>`, …).
fn anonymous_type_str(resolve: &Resolve, kind: &TypeDefKind) -> String {
    let t = |ty: &Type| type_str(resolve, ty);
    match kind {
        TypeDefKind::List(ty) => format!("list<{}>", t(ty)),
        TypeDefKind::FixedLengthList(ty, n) => format!("list<{}, {n}>", t(ty)),
        TypeDefKind::Map(k, v) => format!("map<{}, {}>", t(k), t(v)),
        TypeDefKind::Option(ty) => format!("option<{}>", t(ty)),
        TypeDefKind::Result(r) => match (&r.ok, &r.err) {
            (None, None) => "result".into(),
            (Some(ok), None) => format!("result<{}>", t(ok)),
            (None, Some(err)) => format!("result<_, {}>", t(err)),
            (Some(ok), Some(err)) => format!("result<{}, {}>", t(ok), t(err)),
        },
        TypeDefKind::Tuple(tuple) => format!(
            "tuple<{}>",
            tuple.types.iter().map(t).collect::<Vec<_>>().join(", ")
        ),
        TypeDefKind::Handle(Handle::Own(r)) => resource_name(resolve, *r),
        TypeDefKind::Handle(Handle::Borrow(r)) => format!("borrow<{}>", resource_name(resolve, *r)),
        TypeDefKind::Future(ty) => match ty {
            Some(ty) => format!("future<{}>", t(ty)),
            None => "future".into(),
        },
        TypeDefKind::Stream(ty) => match ty {
            Some(ty) => format!("stream<{}>", t(ty)),
            None => "stream".into(),
        },
        TypeDefKind::Type(ty) => t(ty),
        // Named-only kinds never appear anonymously in a resolved package.
        TypeDefKind::Record(_)
        | TypeDefKind::Resource
        | TypeDefKind::Flags(_)
        | TypeDefKind::Variant(_)
        | TypeDefKind::Enum(_)
        | TypeDefKind::Unknown => "<anonymous>".into(),
    }
}

/// A resource's name, following `use` aliases to the definition.
fn resource_name(resolve: &Resolve, id: wit_parser::TypeId) -> String {
    let def = &resolve.types[id];
    match (&def.name, &def.kind) {
        (Some(name), _) => name.clone(),
        (None, TypeDefKind::Type(Type::Id(inner))) => resource_name(resolve, *inner),
        _ => "<resource>".into(),
    }
}

/// `(ApiFunctionKind literal, is_async)` for a function.
fn function_kind(resolve: &Resolve, kind: &FunctionKind) -> (String, bool) {
    let owner = |id: &wit_parser::TypeId| lit(&resource_name(resolve, *id));
    match kind {
        FunctionKind::Freestanding => ("ApiFunctionKind::Freestanding".into(), false),
        FunctionKind::AsyncFreestanding => ("ApiFunctionKind::Freestanding".into(), true),
        FunctionKind::Method(r) => (
            format!("ApiFunctionKind::Method({}.to_string())", owner(r)),
            false,
        ),
        FunctionKind::AsyncMethod(r) => (
            format!("ApiFunctionKind::Method({}.to_string())", owner(r)),
            true,
        ),
        FunctionKind::Static(r) => (
            format!("ApiFunctionKind::Static({}.to_string())", owner(r)),
            false,
        ),
        FunctionKind::AsyncStatic(r) => (
            format!("ApiFunctionKind::Static({}.to_string())", owner(r)),
            true,
        ),
        FunctionKind::Constructor(r) => (
            format!("ApiFunctionKind::Constructor({}.to_string())", owner(r)),
            false,
        ),
    }
}

/// The `ApiTypeKind` literal for a named type definition.
fn type_kind(resolve: &Resolve, kind: &TypeDefKind) -> String {
    let field = |name: &str, ty: Option<&Type>, doc: Option<&str>| {
        format!(
            "ApiMember {{ name: {}.to_string(), ty: {}, doc: {} }}",
            lit(name),
            opt_lit(ty.map(|ty| type_str(resolve, ty)).as_deref()),
            opt_lit(doc),
        )
    };
    let members = |items: Vec<String>| items.join(", ");
    match kind {
        TypeDefKind::Record(r) => format!(
            "ApiTypeKind::Record(vec![{}])",
            members(
                r.fields
                    .iter()
                    .map(|f| field(&f.name, Some(&f.ty), f.docs.contents.as_deref()))
                    .collect()
            )
        ),
        TypeDefKind::Variant(v) => format!(
            "ApiTypeKind::Variant(vec![{}])",
            members(
                v.cases
                    .iter()
                    .map(|c| field(&c.name, c.ty.as_ref(), c.docs.contents.as_deref()))
                    .collect()
            )
        ),
        TypeDefKind::Enum(e) => format!(
            "ApiTypeKind::Enum(vec![{}])",
            members(
                e.cases
                    .iter()
                    .map(|c| field(&c.name, None, c.docs.contents.as_deref()))
                    .collect()
            )
        ),
        TypeDefKind::Flags(f) => format!(
            "ApiTypeKind::Flags(vec![{}])",
            members(
                f.flags
                    .iter()
                    .map(|c| field(&c.name, None, c.docs.contents.as_deref()))
                    .collect()
            )
        ),
        TypeDefKind::Resource => "ApiTypeKind::Resource".into(),
        other => format!(
            "ApiTypeKind::Alias({}.to_string())",
            lit(&anonymous_type_str(resolve, other))
        ),
    }
}

/// A Rust string literal for `s` (Debug formatting escapes quotes/newlines).
fn lit(s: &str) -> String {
    format!("{s:?}")
}

/// `None` or `Some("...".to_string())`.
fn opt_lit(s: Option<&str>) -> String {
    match s {
        Some(s) => format!("Some({}.to_string())", lit(s)),
        None => "None".to_string(),
    }
}

/// Comma-separated `"a".to_string(), "b".to_string()` for a `vec![...]`.
fn str_vec(items: &[String]) -> String {
    items
        .iter()
        .map(|s| format!("{}.to_string()", lit(s)))
        .collect::<Vec<_>>()
        .join(", ")
}
