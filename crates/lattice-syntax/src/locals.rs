//! `locals.scm`: which names a language's code defines, where it reads
//! them, and the scopes they live in (RF.4h, refactor.md §9.1).
//!
//! Language knowledge more than one feature reads: native extract asks
//! whether an expression reads a name defined inside the statement it would
//! be lifted above, native inline finds a variable's uses, and a future
//! local-symbol highlight or server-less `gd` reads the same facts. So the
//! queries live here, beside `textobjects.scm`, compiled once into the
//! registry ([`crate::registry::LangRegistry::locals_query`]) and handed to
//! plugins as that compiled query (`tree-snapshot.language-query("locals")`),
//! so no plugin ships its own copy or recompiles it.
//!
//! # The capture contract
//!
//! | capture | meaning |
//! |---|---|
//! | `@local.scope` | a node that opens a scope for the names defined in it |
//! | `@local.definition` | an identifier that introduces a local name |
//! | `@local.reference` | an identifier that may read a name: a catch-all on the grammar's identifier kind |
//! | `@local.member` | an identifier in a member or label position (`obj.x`, `f(x=1)`, `{k = v}`) |
//!
//! **A node captured as `@local.definition` or `@local.member` is not a
//! reference**, though the catch-all matches it too. A query cannot say "an
//! identifier, except as a field's name" (tree-sitter has no negated child
//! pattern, and the seam evaluates only the text predicates), so the
//! exclusion is the consumer's: take the reference set minus the other two.
//! `@local.member` exists because in Python, Ruby, Lua, Java and Go's keyed
//! literals a member name has the SAME node kind as a variable. Without it,
//! inlining `x` would rewrite `obj.x`.
//!
//! The files are written for lattice against the pinned grammars, not
//! vendored (decided 2026-11-16): MIT throughout, and each is tested here
//! against the grammar it compiles for. They cover the languages refactoring
//! acts on; a language with no file has no locals, and a consumer refuses it
//! by name.

const RUST_LOCALS_QUERY: &str = include_str!("../queries/rust/locals.scm");
const PYTHON_LOCALS_QUERY: &str = include_str!("../queries/python/locals.scm");
const RUBY_LOCALS_QUERY: &str = include_str!("../queries/ruby/locals.scm");
const GO_LOCALS_QUERY: &str = include_str!("../queries/go/locals.scm");
const JAVASCRIPT_LOCALS_QUERY: &str = include_str!("../queries/javascript/locals.scm");
const TYPESCRIPT_LOCALS_QUERY: &str = include_str!("../queries/typescript/locals.scm");
const CPP_LOCALS_QUERY: &str = include_str!("../queries/cpp/locals.scm");
const JAVA_LOCALS_QUERY: &str = include_str!("../queries/java/locals.scm");
const LUA_LOCALS_QUERY: &str = include_str!("../queries/lua/locals.scm");

/// The capture names, for consumers that match on them.
pub const SCOPE: &str = "local.scope";
/// See [`SCOPE`].
pub const DEFINITION: &str = "local.definition";
/// See [`SCOPE`].
pub const REFERENCE: &str = "local.reference";
/// See [`SCOPE`].
pub const MEMBER: &str = "local.member";

/// The languages that ship a `locals.scm`, by registry name.
pub const LANGUAGES: &[&str] = &[
    "rust",
    "python",
    "ruby",
    "go",
    "javascript",
    "typescript",
    "tsx",
    "cpp",
    "java",
    "lua",
];

/// `locals.scm`'s source for the language registered as `name`, or `None`
/// for one without. Keyed by name, as `indents_source` is, so
/// `build_config` does not grow a parameter per query. TSX shares
/// TypeScript's: its grammar is a superset.
pub fn source(name: &str) -> Option<&'static str> {
    Some(match name {
        "rust" => RUST_LOCALS_QUERY,
        "python" => PYTHON_LOCALS_QUERY,
        "ruby" => RUBY_LOCALS_QUERY,
        "go" => GO_LOCALS_QUERY,
        "javascript" => JAVASCRIPT_LOCALS_QUERY,
        "typescript" | "tsx" => TYPESCRIPT_LOCALS_QUERY,
        "cpp" => CPP_LOCALS_QUERY,
        "java" => JAVA_LOCALS_QUERY,
        "lua" => LUA_LOCALS_QUERY,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use std::collections::BTreeSet;

    use streaming_iterator::StreamingIterator;
    use tree_sitter::{Parser, QueryCursor};

    use super::{DEFINITION, LANGUAGES, MEMBER, REFERENCE, SCOPE, source};
    use crate::registry::LangRegistry;

    /// What a language's locals say about `src`: the texts captured as
    /// definitions, as members, and as references (the catch-all minus the
    /// other two, as the contract says), and how many scopes.
    #[derive(Debug, Default)]
    struct Locals {
        definitions: BTreeSet<String>,
        references: BTreeSet<String>,
        members: BTreeSet<String>,
        scopes: usize,
    }

    fn locals(lang: &str, src: &str) -> Locals {
        let registry = LangRegistry::standard().expect("registry");
        let language = registry.tree_sitter_language(lang).expect("grammar");
        let query = registry.locals_query(lang).expect("a locals query");
        let mut parser = Parser::new();
        parser.set_language(&language).expect("language");
        let tree = parser.parse(src, None).expect("parse");
        assert!(!tree.root_node().has_error(), "{lang} fixture parses");
        let names = query.capture_names();
        let mut cursor = QueryCursor::new();
        let mut matches = cursor.matches(&query, tree.root_node(), src.as_bytes());
        let mut defs = BTreeSet::new();
        let mut members = BTreeSet::new();
        let mut refs = Vec::new();
        let mut out = Locals::default();
        while let Some(m) = matches.next() {
            for cap in m.captures {
                let range = cap.node.byte_range();
                let text = src[range.clone()].to_string();
                match names[cap.index as usize] {
                    SCOPE => out.scopes += 1,
                    DEFINITION => {
                        defs.insert((range.start, range.end));
                        out.definitions.insert(text);
                    }
                    MEMBER => {
                        members.insert((range.start, range.end));
                        out.members.insert(text);
                    }
                    REFERENCE => refs.push(((range.start, range.end), text)),
                    other => panic!("{lang}: capture @{other} is not in the contract"),
                }
            }
        }
        out.references = refs
            .into_iter()
            .filter(|(r, _)| !defs.contains(r) && !members.contains(r))
            .map(|(_, t)| t)
            .collect();
        out
    }

    fn set(words: &[&str]) -> BTreeSet<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    /// Assert `expected ⊆ actual`, naming what is missing.
    fn has(lang: &str, what: &str, actual: &BTreeSet<String>, expected: &[&str]) {
        let missing: Vec<_> = set(expected).difference(actual).cloned().collect();
        assert!(
            missing.is_empty(),
            "{lang} {what}: missing {missing:?} in {actual:?}"
        );
    }

    fn lacks(lang: &str, what: &str, actual: &BTreeSet<String>, absent: &[&str]) {
        let present: Vec<_> = set(absent).intersection(actual).cloned().collect();
        assert!(present.is_empty(), "{lang} {what}: unexpected {present:?}");
    }

    #[test]
    fn every_listed_language_compiles_its_locals() {
        let registry = LangRegistry::standard().expect("registry");
        for lang in LANGUAGES {
            assert!(source(lang).is_some(), "{lang} has a source");
            assert!(
                registry.locals_query(lang).is_some(),
                "{lang}'s locals.scm compiles into the registry"
            );
        }
    }

    #[test]
    fn a_language_without_a_file_has_no_locals() {
        let registry = LangRegistry::standard().expect("registry");
        for lang in ["c", "bash", "json", "markdown"] {
            assert!(source(lang).is_none());
            assert!(registry.locals_query(lang).is_none(), "{lang}");
        }
        assert!(registry.locals_query("no-such-language").is_none());
    }

    #[test]
    fn rust() {
        let l = locals(
            "rust",
            "fn f(a: i32) -> i32 {\n    let (b, mut c) = (a, 1);\n    let s = S { x: b };\n    let g = |y| y + c;\n    match s { S { x } => x + g(c) }\n}\n",
        );
        has(
            "rust",
            "definitions",
            &l.definitions,
            &["f", "a", "b", "c", "s", "g", "y", "x"],
        );
        has(
            "rust",
            "references",
            &l.references,
            &["a", "b", "c", "s", "g", "y", "x"],
        );
        assert!(l.scopes >= 4, "fn, block, closure, arm: {}", l.scopes);
    }

    #[test]
    fn python() {
        let l = locals(
            "python",
            "import os.path\nfrom a import b as c\ndef f(x, y=1, *args, **kw):\n    z = x.attr + y\n    for i in range(3):\n        pass\n    return [w for w in args]\ng = f(x=2)\n",
        );
        has(
            "python",
            "definitions",
            &l.definitions,
            &["os", "c", "f", "x", "y", "args", "kw", "z", "i", "w", "g"],
        );
        has("python", "members", &l.members, &["attr", "x"]);
        has(
            "python",
            "references",
            &l.references,
            &["x", "y", "args", "range", "f", "w"],
        );
        lacks("python", "references", &l.references, &["attr"]);
    }

    #[test]
    fn ruby() {
        let l = locals(
            "ruby",
            "def m(a, b = 1, *c, d:, **e, &f)\n  g = a + b\n  [1].each { |h| h }\n  h2, i = 1, 2\nrescue => err\n  err\nend\n",
        );
        has(
            "ruby",
            "definitions",
            &l.definitions,
            &["a", "b", "c", "d", "e", "f", "g", "h", "h2", "i", "err"],
        );
        has("ruby", "members", &l.members, &["each"]);
        has("ruby", "references", &l.references, &["a", "b", "h", "err"]);
        lacks("ruby", "references", &l.references, &["each"]);
    }

    #[test]
    fn go() {
        let l = locals(
            "go",
            "package p\n\nfunc f(a int, b ...int) int {\n\tc := a\n\tvar d = 1\n\tfor i, v := range b {\n\t\t_ = v\n\t\tc += i\n\t}\n\tq := Point{X: c}\n\treturn c + d + q.X\n}\n",
        );
        has(
            "go",
            "definitions",
            &l.definitions,
            &["f", "a", "b", "c", "d", "i", "v", "q"],
        );
        has("go", "members", &l.members, &["X"]);
        has(
            "go",
            "references",
            &l.references,
            &["a", "b", "c", "d", "i", "v", "q"],
        );
        lacks("go", "references", &l.references, &["X"]);
    }

    #[test]
    fn javascript() {
        let l = locals(
            "javascript",
            "import d, { a, b as c } from \"m\";\nimport * as ns from \"n\";\nfunction f(x, y = 1, ...rest) {\n  const { p, q: r } = x;\n  let [s] = rest;\n  const g = (z) => z + p;\n  return { r, s };\n}\ntry {} catch (e) {}\nfor (const k of []) {}\n",
        );
        has(
            "javascript",
            "definitions",
            &l.definitions,
            &[
                "d", "a", "c", "ns", "f", "x", "y", "rest", "p", "r", "s", "g", "z", "e", "k",
            ],
        );
        lacks("javascript", "definitions", &l.definitions, &["b"]);
        has(
            "javascript",
            "references",
            &l.references,
            &["x", "rest", "z", "p", "r", "s"],
        );
    }

    #[test]
    fn typescript_and_tsx() {
        for lang in ["typescript", "tsx"] {
            let l = locals(
                lang,
                "function f(x: number, y?: string): number {\n  const z = x + 1;\n  return z;\n}\nclass C {}\n",
            );
            has(
                lang,
                "definitions",
                &l.definitions,
                &["f", "x", "y", "z", "C"],
            );
            has(lang, "references", &l.references, &["x", "z"]);
        }
    }

    #[test]
    fn cpp() {
        let l = locals(
            "cpp",
            "int f(int a, int* b, int& c) {\n  int d = a;\n  int* e = b;\n  auto [g, h] = pair();\n  for (int i : v) { d += i; }\n  auto l = [](int m) { return m; };\n  return d + c;\n}\n",
        );
        has(
            "cpp",
            "definitions",
            &l.definitions,
            &["f", "a", "b", "c", "d", "e", "g", "h", "i", "l", "m"],
        );
        has(
            "cpp",
            "references",
            &l.references,
            &["a", "b", "c", "d", "i", "m"],
        );
    }

    #[test]
    fn java() {
        let l = locals(
            "java",
            "class A {\n  int f(int a, String... b) {\n    int c = a;\n    for (String s : b) { c += s.length(); }\n    try (var r = open()) {} catch (Exception e) {}\n    Runnable g = () -> {};\n    Function<Integer, Integer> h = x -> x + c;\n    return this.c + c;\n  }\n}\n",
        );
        has(
            "java",
            "definitions",
            &l.definitions,
            &["f", "a", "b", "c", "s", "r", "e", "g", "h", "x"],
        );
        has("java", "members", &l.members, &["length", "c", "open"]);
        has(
            "java",
            "references",
            &l.references,
            &["a", "b", "c", "s", "x"],
        );
        lacks("java", "references", &l.references, &["length", "open"]);
    }

    #[test]
    fn lua() {
        let l = locals(
            "lua",
            "local a, b = 1, 2\nlocal function f(x, ...)\n  local t = { k = x }\n  for i = 1, 3 do a = a + i end\n  for _, v in ipairs(t) do print(v) end\n  return t.k + x\nend\n",
        );
        has(
            "lua",
            "definitions",
            &l.definitions,
            &["a", "b", "f", "x", "t", "i", "v"],
        );
        has("lua", "members", &l.members, &["k"]);
        has(
            "lua",
            "references",
            &l.references,
            &["a", "x", "t", "i", "v", "ipairs"],
        );
        lacks("lua", "references", &l.references, &["k"]);
    }
}
