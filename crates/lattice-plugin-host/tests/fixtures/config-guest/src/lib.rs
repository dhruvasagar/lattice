//! PH7.10 config fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `config-plugin` world:
//!   - `register-options` (the world export the host calls once) declares three
//!     options — one of each type — via the imported `config.register-option`,
//!     then reads one back via `config.get-option` and appends its value to
//!     `/data/option.log` (the writable data-dir mount, PH7.2) so the host test
//!     can observe the declare→register→read round-trip end to end.
//!
//! Declaring via the raw WIT calls (no Rust SDK) is deliberate: it exercises the
//! CANONICAL, language-agnostic surface any component-model language uses. The
//! Rust `#[derive(PluginOption)]` ergonomics (PH7.10b) expand to these same calls.

wit_bindgen::generate!({
    world: "config-plugin",
    path: "../../../../../wit",
});

use lattice::plugin_host::config;
use lattice::plugin_host::config::OptionType;
use lattice_plugin_sdk::{OptionKind, PluginOption, parse_option};

struct Component;

/// Whether the fixture is enabled.
#[derive(PluginOption)]
#[option(name = "enabled", default = "true")]
struct Enabled(bool);

/// How many things the fixture tracks.
#[derive(PluginOption)]
#[option(name = "count", default = "3")]
struct Count(i64);

/// A display label.
#[derive(PluginOption)]
#[option(name = "label", default = "hello")]
struct Label(String);

/// Map the SDK's WIT-agnostic [`OptionKind`] to the generated `option-type` — the
/// one-line approach-A tax a plugin pays (the SDK can't name the per-world WIT
/// type). Done once, not per option.
fn wit_ty(kind: OptionKind) -> OptionType {
    match kind {
        OptionKind::Boolean => OptionType::Boolean,
        OptionKind::Integer => OptionType::Integer,
        OptionKind::String => OptionType::String,
    }
}

/// Register one derived option through the `config` wire using its SDK metadata.
fn register<O: PluginOption>() {
    config::register_option(O::NAME, wit_ty(O::KIND), O::DEFAULT, O::DOC);
}

impl Guest for Component {
    /// The host calls this once; the guest declares its options via the SDK
    /// derive (`NAME`/`KIND`/`DEFAULT`/`DOC`) over the imported `register-option`,
    /// then reads one back and parses it typed via `parse_option`.
    fn register_options() {
        register::<Enabled>();
        register::<Count>();
        register::<Label>();

        // CI.7: SET an option value through the `set-option` seam (init.rs's
        // config front-end), then read it back — proving the guest can override a
        // value, not only declare one.
        let _ = config::set_option(Count::NAME, "5");

        // Read `count` back through `get-option` and parse it typed (i64) via the
        // SDK — the full declare→set→read→parse round-trip. Record it so the host
        // test can observe the SET value crossed correctly.
        let raw = config::get_option(Count::NAME).unwrap_or_default();
        let count = parse_option::<Count>(&raw).unwrap_or(-1);
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/data/option.log")
        {
            let _ = writeln!(f, "count={count}");
        }

        // ── TC.3: an option whose value has STRUCTURE ────────────────
        //
        // org's `capture-templates` reduced to its shape: a LIST of
        // RECORDS, one field of which is itself a record. Declared
        // through the raw WIT calls, no SDK — the same reason the
        // scalar declarations above do: this is the canonical,
        // language-agnostic surface, and TC.4's derive expands to
        // exactly these calls.
        //
        // Written as an ARENA because WIT has no recursive types. The
        // hand-built indices are what makes TC.4 worth having; they are
        // also why this fixture builds one by hand at least once, so
        // the encoding is exercised without the generator that will
        // later hide it.
        let _ = config::register_structured_option(
            "templates",
            &config::ConfigSchema {
                nodes: vec![
                    // 0: string
                    config::SchemaNode::Scalar(config::OptionType::String),
                    // 1: record { file: string }
                    config::SchemaNode::Record(vec![config::SchemaField {
                        name: "file".to_string(),
                        schema: 0,
                        required: true,
                        doc: "where it lands".to_string(),
                    }]),
                    // 2: record { key, target, body? }
                    config::SchemaNode::Record(vec![
                        config::SchemaField {
                            name: "key".to_string(),
                            schema: 0,
                            required: true,
                            doc: "the key to press".to_string(),
                        },
                        config::SchemaField {
                            name: "target".to_string(),
                            schema: 1,
                            required: true,
                            doc: "where the capture goes".to_string(),
                        },
                        config::SchemaField {
                            name: "body".to_string(),
                            schema: 0,
                            required: false,
                            doc: "the template body".to_string(),
                        },
                    ]),
                    // 3: list<2>
                    config::SchemaNode::ListOf(2),
                ],
                root: 3,
            },
            // Default: no templates. An empty list is a legal value of
            // `list<record>`, which is what lets the option register
            // before anyone has configured it.
            &config::ConfigValue {
                nodes: vec![config::ValueNode::List(vec![])],
                root: 0,
            },
            "Capture templates (TC.3 fixture).",
        );

        // Set one through the typed seam, then read it back the same
        // way. Recording what came BACK — not what went in — is the
        // point: a seam that accepted the tree and stored a mangled one
        // would pass any assertion made on the write alone.
        let _ = config::set_option_value(
            "templates",
            &config::ConfigValue {
                nodes: vec![
                    config::ValueNode::String("t".to_string()),           // 0
                    config::ValueNode::String("~/org/refile.org".to_string()), // 1
                    config::ValueNode::Record(vec![("file".to_string(), 1)]),  // 2
                    config::ValueNode::Record(vec![
                        ("key".to_string(), 0),
                        ("target".to_string(), 2),
                    ]), // 3
                    config::ValueNode::List(vec![3]),                     // 4
                ],
                root: 4,
            },
        );

        if let Some(v) = config::get_option_value("templates") {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("/data/option.log")
                .map(|mut f| writeln!(f, "templates={}", describe(&v)));
        }

        // A tree that does NOT fit the schema must be refused, and the
        // refusal must not disturb the value already set. Logged so the
        // host test can assert BOTH halves — a seam that rejected the
        // write but cleared the option would satisfy "returns false".
        let rejected = config::set_option_value(
            "templates",
            &config::ConfigValue {
                nodes: vec![
                    config::ValueNode::Int(7), // `key` is a string
                    config::ValueNode::Record(vec![("key".to_string(), 0)]),
                    config::ValueNode::List(vec![1]),
                ],
                root: 2,
            },
        );
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/data/option.log")
            .map(|mut f| writeln!(f, "rejected={rejected}"));
    }
}

/// Flatten a value arena to one line the host test can assert on, resolving
/// indices so the LINK structure is part of what is checked rather than only
/// the node contents.
fn describe(v: &config::ConfigValue) -> String {
    fn go(v: &config::ConfigValue, i: u32) -> String {
        let Some(node) = v.nodes.get(i as usize) else {
            return "<oob>".to_string();
        };
        match node {
            config::ValueNode::Bool(b) => b.to_string(),
            config::ValueNode::Int(n) => n.to_string(),
            config::ValueNode::String(s) => s.clone(),
            config::ValueNode::List(items) => {
                let inner: Vec<String> = items.iter().map(|c| go(v, *c)).collect();
                format!("[{}]", inner.join(","))
            }
            config::ValueNode::Record(fields) => {
                let inner: Vec<String> = fields
                    .iter()
                    .map(|(k, c)| format!("{k}={}", go(v, *c)))
                    .collect();
                format!("{{{}}}", inner.join(","))
            }
        }
    }
    go(v, v.root)
}

export!(Component);
