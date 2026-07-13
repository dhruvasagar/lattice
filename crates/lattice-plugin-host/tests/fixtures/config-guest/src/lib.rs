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

struct Component;

impl Guest for Component {
    /// The host calls this once; the guest declares its options through the
    /// imported `config.register-option` host function, then reads one back.
    fn register_options() {
        config::register_option(
            "config-fixture.enabled",
            OptionType::Boolean,
            "true",
            "whether the fixture is enabled",
        );
        config::register_option(
            "config-fixture.count",
            OptionType::Integer,
            "3",
            "how many things the fixture tracks",
        );
        config::register_option(
            "config-fixture.label",
            OptionType::String,
            "hello",
            "a display label",
        );

        // Read one back through `get-option` (the registry round-trip) and record
        // it so the host test can observe the value crossed correctly.
        let read = config::get_option("config-fixture.count").unwrap_or_default();
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/data/option.log")
        {
            let _ = writeln!(f, "count={read}");
        }
    }
}

export!(Component);
