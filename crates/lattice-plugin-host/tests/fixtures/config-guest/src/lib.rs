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
#[option(name = "config-fixture.enabled", default = "true")]
struct Enabled(bool);

/// How many things the fixture tracks.
#[derive(PluginOption)]
#[option(name = "config-fixture.count", default = "3")]
struct Count(i64);

/// A display label.
#[derive(PluginOption)]
#[option(name = "config-fixture.label", default = "hello")]
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

        // Read `count` back through `get-option` and parse it typed (i64) via the
        // SDK — the full declare→register→read→parse round-trip. Record it so the
        // host test can observe the value crossed correctly.
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
    }
}

export!(Component);
