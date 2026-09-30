//! The guest-side Rust SDK for lattice plugins: typed event payloads, typed
//! options and typed configuration shapes layered over the plugin-host WIT
//! wire. Compiled INTO plugins (Rust today; other component-model languages
//! use the WIT directly), never into the host.
//!
//! ## What it owns
//!
//! The WIT is the plugin API; this crate adds **zero** capability that is not
//! on the wire — only ergonomics a Rust author would otherwise hand-write. The
//! plugin-host `emit-event` / `register-event` host-services (PH7.8b.2) carry
//! `name: string` + `payload: list<u8>` — opaque MessagePack the host never
//! interprets. That is deliberate (the boundary discipline the whole host rests
//! on), but raw bytes are a poor author API. This crate adds the type-safe layer:
//!
//!   - [`PluginEvent`] — a trait pairing a compile-time `NAME` + `DOC` with
//!     MessagePack `encode` / `decode`.
//!   - `#[derive(PluginEvent)]` — derives all four from a serde struct: `DOC`
//!     from the struct's `///` doc-comment (the doc-comment IS the event doc),
//!     `NAME` from `#[event(name = "...")]` or the kebab-cased type name.
//!   - [`try_decode`] — the subscriber-side helper: name-gate + decode in one.
//!   - [`PluginOption`] + `#[derive(PluginOption)]` + [`parse_option`] — the
//!     same shape for scalar options (`bool` / `i64` / `String`).
//!   - [`shape`] — [`shape::ConfigShape`] + `#[derive(ConfigShape)]`: a Rust
//!     struct as a structured config schema and value, and the arena
//!     flattening ([`shape::flatten_schema`], [`shape::flatten_value`],
//!     [`shape::unflatten_value`]) the WIT seam needs.
//!
//! ## What it must not depend on
//!
//! No `lattice-*` runtime crate and no `wit-bindgen` bindings — only `serde`,
//! `rmp-serde` and its own derive. Two structural reasons: it is published and
//! versioned for out-of-tree plugin authors, so it cannot drag the editor in;
//! and it must compose with EVERY plugin world, which it can only do by naming
//! none of their generated types. It is a separate crate because it is the one
//! piece of lattice that compiles into guests.
//!
//! ## WIT-agnostic by design (approach A)
//!
//! This crate touches **no** plugin-host bindings — it is pure serde + a derive.
//! The host calls stay plugin-side one-liners using the derived constants
//! (`host_services` below stands in for a plugin's generated bindings):
//!
//! ```
//! use lattice_plugin_sdk::{DecodeError, PluginEvent};
//! use serde::{Deserialize, Serialize};
//! # mod host_services {
//! #     pub fn register_event(_name: &str, _doc: &str) {}
//! #     pub fn emit_event(_name: &str, _payload: &[u8]) {}
//! # }
//!
//! /// The indexer finished scanning a file.
//! #[derive(Debug, PartialEq, Serialize, Deserialize, PluginEvent)]
//! #[event(name = "indexer.file-scanned")]
//! struct FileScanned {
//!     path: String,
//!     symbols: u32,
//! }
//!
//! // at register-events:
//! host_services::register_event(FileScanned::NAME, FileScanned::DOC);
//! // to emit:
//! let ev = FileScanned { path: "src/lib.rs".into(), symbols: 42 };
//! let payload = ev.encode();
//! host_services::emit_event(FileScanned::NAME, &payload);
//!
//! // in another plugin's on-event(name, payload):
//! # fn on_event(name: &str, payload: &[u8]) -> Result<Option<FileScanned>, DecodeError> {
//! if let Some(ev) = lattice_plugin_sdk::try_decode::<FileScanned>(name, payload) {
//!     let ev = ev?; // a real FileScanned
//!     return Ok(Some(ev));
//! }
//! # Ok(None)
//! # }
//! assert_eq!(on_event("indexer.file-scanned", &payload), Ok(Some(ev)));
//! assert_eq!(FileScanned::DOC, "The indexer finished scanning a file.");
//! ```
//!
//! Because the SDK is world-agnostic it composes with EVERY plugin world (events,
//! grammar, completion, …) unchanged — it is the seed the other SDK seams reuse.
//! A fuller `ctx.emit(ev)` / `on_event::<E>()` sugar can layer on once a real
//! multi-world plugin exists to shape the host-call binding.
//!
//! ## Cross-plugin contracts
//!
//! Because a `PluginEvent` type is just a serde struct, plugin A can publish its
//! event types in a shared crate and plugin B can depend on it — a
//! compile-checked, versioned event contract (the coordinating-plugins use case).
//!
//! ## Design
//!
//! - `docs/dev/architecture/plugin-host.md` — the host the wire talks to, and
//!   the events / config seams this crate types.
//! - `docs/dev/architecture/typed-configuration.md` — [`shape`] and the arena
//!   encoding.
//! - `docs/dev/guides/plugin-authoring.md` — end-to-end plugin authoring.

#![warn(missing_docs)]

// So the derive's generated `::lattice_plugin_sdk::..` paths resolve inside this
// crate's own tests (the `lattice-config` / serde precedent for a crate that
// consumes its own derive).
extern crate self as lattice_plugin_sdk;

pub use lattice_plugin_sdk_derive::{ConfigShape, PluginEvent, PluginOption};

pub mod shape;

/// A plugin-defined event: a typed view over the opaque `emit-event` /
/// `on-event` wire (PH7.8b.2). Implement via `#[derive(PluginEvent)]` on a
/// serde-serializable struct; hand-implementing is possible but rarely needed.
///
/// `NAME` is the wire identifier (matched by subscribers, registered via
/// `register-event`); `DOC` is the human summary surfaced in `:describe-event`.
/// `encode` / `decode` round-trip the payload as MessagePack.
///
/// The derive requires the struct to implement serde's `Serialize` and
/// `Deserialize`; its `NAME` defaults to the kebab-cased type name
/// (`MyCustomEvent` → `my-custom-event`, acronyms degrade per letter), so real
/// plugins namespace it explicitly with `#[event(name = "plugin.event")]`.
///
/// # Examples
///
/// ```
/// use lattice_plugin_sdk::PluginEvent;
/// use serde::{Deserialize, Serialize};
///
/// /// Kebab-name fallback event.
/// #[derive(Debug, PartialEq, Serialize, Deserialize, PluginEvent)]
/// struct MyCustomEvent {
///     value: i64,
/// }
///
/// assert_eq!(MyCustomEvent::NAME, "my-custom-event");
/// assert_eq!(MyCustomEvent::DOC, "Kebab-name fallback event.");
///
/// let bytes = MyCustomEvent { value: 7 }.encode();
/// assert_eq!(MyCustomEvent::decode(&bytes), Ok(MyCustomEvent { value: 7 }));
/// // A payload for some other type is a typed error, never a panic.
/// assert!(MyCustomEvent::decode(&[0xc0]).is_err());
/// ```
pub trait PluginEvent: Sized {
    /// The event's wire name — the identifier crossed to `emit-event` and
    /// matched by subscribers (e.g. `"git.hunks-changed"`).
    const NAME: &'static str;
    /// The human-facing doc (from the struct's `///` comment), shown by
    /// `:describe-event` once registered.
    const DOC: &'static str;

    /// Serialize to the opaque MessagePack payload `emit-event` carries.
    ///
    /// # Panics
    ///
    /// The derived impl panics only if the type's `Serialize` impl itself
    /// errors, which a plain derived serde struct never does; a hand-written
    /// `Serialize` that can fail is treated as a programming bug.
    fn encode(&self) -> Vec<u8>;

    /// Deserialize from a payload received on `on-event`. A malformed / mistyped
    /// payload is a typed [`DecodeError`], never a panic.
    fn decode(bytes: &[u8]) -> Result<Self, DecodeError>;
}

/// A failed [`PluginEvent::decode`] — the payload was not valid MessagePack for
/// the target type (wrong event type, version skew, corruption). Carries the
/// underlying decoder message; opaque and stable (it hides the serde impl).
///
/// `Display` renders `plugin event decode failed: <message>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(
    /// The decoder's own message. Diagnostic text for logs, not a stable
    /// format to match on.
    pub String,
);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plugin event decode failed: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// Subscriber-side helper: if `name` names event `E`, decode `payload` into it;
/// otherwise `None` (the event is for a different subscriber). Folds the
/// name-gate the guest would otherwise write by hand in `on-event` into one call.
///
/// The name match is exact (case-sensitive, no prefix matching); when it fails
/// the payload is not looked at.
///
/// # Examples
///
/// ```
/// use lattice_plugin_sdk::{PluginEvent, try_decode};
/// use serde::{Deserialize, Serialize};
///
/// /// A project finished indexing.
/// #[derive(Debug, PartialEq, Serialize, Deserialize, PluginEvent)]
/// #[event(name = "indexer.indexed")]
/// struct Indexed { files: u32 }
///
/// let payload = Indexed { files: 3 }.encode();
/// match try_decode::<Indexed>("indexer.indexed", &payload) {
///     Some(Ok(ev)) => assert_eq!(ev.files, 3),
///     Some(Err(e)) => panic!("our event, but a bad payload: {e}"),
///     None => panic!("not our event"),
/// }
/// // Some other plugin's event: not decoded at all.
/// assert_eq!(try_decode::<Indexed>("git.hunks-changed", &payload), None);
/// // Our name, garbage bytes: a typed error.
/// assert!(matches!(try_decode::<Indexed>("indexer.indexed", &[0xc1]), Some(Err(_))));
/// ```
pub fn try_decode<E: PluginEvent>(name: &str, payload: &[u8]) -> Option<Result<E, DecodeError>> {
    (name == E::NAME).then(|| E::decode(payload))
}

/// A plugin-defined scalar option — a typed view over the `config`
/// register/read wire (slice PH7.10b). Implement via `#[derive(PluginOption)]`
/// on a newtype over `bool` / `i64` / `String`; `#[option(default = "...")]` is
/// required, `#[option(name = "...")]` defaults to the kebab-cased type name.
/// For structured (record / list / enum) options use [`shape::ConfigShape`]
/// instead.
///
/// It is **WIT-agnostic** (approach A): the derive only supplies these constants
/// plus the value type. The plugin makes the `config.register-option` /
/// `config.get-option` WIT calls itself, mapping [`OptionKind`] to the generated
/// `option-type` (`config` below stands in for a plugin's generated bindings):
///
/// ```
/// use lattice_plugin_sdk::{OptionKind, PluginOption, parse_option};
/// # mod config {
/// #     pub enum OptionType { Boolean, Integer, String }
/// #     pub fn register_option(_: &str, _: OptionType, _: &str, _: &str) {}
/// #     pub fn get_option(_: &str) -> Option<String> { Some("5".into()) }
/// # }
///
/// /// How many things the plugin tracks.
/// #[derive(PluginOption)]
/// #[option(name = "myplugin.count", default = "3")]
/// struct Count(i64);
///
/// // The one per-plugin mapping to the generated WIT enum.
/// fn wit_ty(kind: OptionKind) -> config::OptionType {
///     match kind {
///         OptionKind::Boolean => config::OptionType::Boolean,
///         OptionKind::Integer => config::OptionType::Integer,
///         OptionKind::String => config::OptionType::String,
///     }
/// }
///
/// config::register_option(Count::NAME, wit_ty(Count::KIND), Count::DEFAULT, Count::DOC);
/// let value: i64 = parse_option::<Count>(&config::get_option(Count::NAME).unwrap()).unwrap();
///
/// assert_eq!(value, 5);
/// assert_eq!(Count::NAME, "myplugin.count");
/// assert_eq!(Count::KIND, OptionKind::Integer);
/// assert_eq!(Count::DOC, "How many things the plugin tracks.");
/// ```
pub trait PluginOption {
    /// The option's registry name (matched by `:set`, shown in `:describe-option`).
    const NAME: &'static str;
    /// The human-facing doc (from the struct's `///` comment).
    const DOC: &'static str;
    /// The initial value as a string (parsed host-side via the native `OptionType`).
    const DEFAULT: &'static str;
    /// The value type — maps to the WIT `option-type` when registering.
    const KIND: OptionKind;
    /// The Rust value type (`bool` / `i64` / `String`), parsed from a
    /// `get-option` string via [`parse_option`].
    type Value: std::str::FromStr;
}

/// The value type of a plugin option — the WIT-agnostic mirror of the `config`
/// interface's `option-type` enum. The plugin maps this to the generated
/// `option-type` at the `register-option` call site (the SDK can't name the
/// per-world WIT type — approach A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    /// A `bool` option; the derive picks it for a `bool` field.
    Boolean,
    /// A signed integer option; the derive picks it for an `i64` field.
    Integer,
    /// A free-form string option; the derive picks it for a `String` field.
    String,
}

/// Parse a `get-option` result string into the option's typed value (slice
/// PH7.10b). `get-option` returns the value formatted by the native
/// `OptionType`; this reads it back into `O::Value` via its `FromStr`.
///
/// # Errors
///
/// A malformed string is a typed [`OptionParseError`] carrying the `FromStr`
/// error's message, never a panic.
///
/// # Examples
///
/// ```
/// use lattice_plugin_sdk::{PluginOption, parse_option};
///
/// /// Whether long lines wrap.
/// #[derive(PluginOption)]
/// #[option(default = "true")]
/// struct WrapLines(bool);
///
/// assert_eq!(WrapLines::NAME, "wrap-lines");
/// assert_eq!(parse_option::<WrapLines>("false"), Ok(false));
/// assert!(parse_option::<WrapLines>("yes").is_err());
/// ```
pub fn parse_option<O: PluginOption>(s: &str) -> Result<O::Value, OptionParseError>
where
    <O::Value as std::str::FromStr>::Err: std::fmt::Display,
{
    s.parse::<O::Value>()
        .map_err(|e| OptionParseError(e.to_string()))
}

/// A failed [`parse_option`] — the `get-option` string didn't parse for the
/// option's value type. Carries the underlying parser message.
///
/// `Display` renders `plugin option parse failed: <message>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionParseError(
    /// The value type's `FromStr` error message, e.g. `invalid digit found in
    /// string`. Diagnostic text, not a stable format to match on.
    pub String,
);

impl std::fmt::Display for OptionParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plugin option parse failed: {}", self.0)
    }
}

impl std::error::Error for OptionParseError {}

/// Implementation detail used by the generated `#[derive(PluginEvent)]` code so a
/// consumer depends only on `lattice-plugin-sdk` (the SDK owns the `rmp-serde`
/// dependency, not every plugin). Not part of the stable API.
#[doc(hidden)]
pub mod __private {
    use super::DecodeError;

    /// MessagePack-encode a derived event. Infallible for the derived case (a
    /// plain serde struct never errors on serialize); a hand-rolled `Serialize`
    /// that errors is a programming bug surfaced as a panic, not silent data loss.
    pub fn encode<T: serde::Serialize>(value: &T) -> Vec<u8> {
        rmp_serde::to_vec(value)
            .expect("PluginEvent MessagePack encoding is infallible for derived structs")
    }

    /// MessagePack-decode a derived event, mapping any decoder error to the
    /// SDK's opaque [`DecodeError`].
    pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, DecodeError> {
        rmp_serde::from_slice(bytes).map_err(|e| DecodeError(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    // The `PluginOption` marker newtypes carry their value type for the derive
    // but the tests read only the derived constants, so the field is unused.
    #![allow(clippy::unwrap_used, clippy::panic, dead_code)]

    use super::*;
    use serde::{Deserialize, Serialize};

    /// A file the indexer finished scanning.
    ///
    /// Second doc line.
    #[derive(Debug, PartialEq, Serialize, Deserialize, PluginEvent)]
    #[event(name = "indexer.file-scanned")]
    struct FileScanned {
        path: String,
        symbols: u32,
    }

    /// Kebab-name fallback event.
    #[derive(Debug, PartialEq, Serialize, Deserialize, PluginEvent)]
    struct MyCustomEvent {
        value: i64,
    }

    #[test]
    fn explicit_name_and_doc_come_from_the_attrs() {
        assert_eq!(FileScanned::NAME, "indexer.file-scanned");
        // The multi-line doc-comment is captured, joined, and trimmed.
        assert_eq!(
            FileScanned::DOC,
            "A file the indexer finished scanning.\n\nSecond doc line."
        );
    }

    #[test]
    fn name_defaults_to_the_kebab_cased_type_name() {
        assert_eq!(MyCustomEvent::NAME, "my-custom-event");
        assert_eq!(MyCustomEvent::DOC, "Kebab-name fallback event.");
    }

    #[test]
    fn encode_decode_round_trips() {
        let ev = FileScanned {
            path: "src/lib.rs".into(),
            symbols: 42,
        };
        let bytes = ev.encode();
        let back = FileScanned::decode(&bytes).unwrap();
        assert_eq!(ev, back, "MessagePack round-trips the struct");
    }

    #[test]
    fn try_decode_gates_on_the_event_name() {
        let ev = FileScanned {
            path: "a.rs".into(),
            symbols: 1,
        };
        let payload = ev.encode();

        // Matching name → Some(Ok(..)).
        let got = try_decode::<FileScanned>("indexer.file-scanned", &payload);
        assert_eq!(got, Some(Ok(ev)));

        // Different name → None (not this subscriber's event; no decode attempted).
        assert_eq!(try_decode::<FileScanned>("other.event", &payload), None);
    }

    #[test]
    fn decode_of_a_bad_payload_is_a_typed_error() {
        // Garbage bytes that are not valid MessagePack for the struct.
        let err = FileScanned::decode(&[0xff, 0x00, 0x01]).unwrap_err();
        assert!(
            format!("{err}").contains("decode failed"),
            "decode surfaces a typed error, never a panic: {err}"
        );
    }

    /// How wide a tab is rendered.
    #[derive(PluginOption)]
    #[option(name = "editor.tab-width", default = "8")]
    struct TabWidth(i64);

    /// Whether long lines wrap.
    #[derive(PluginOption)]
    #[option(default = "true")]
    struct WrapLines(bool);

    #[test]
    fn option_derive_captures_name_doc_default_and_kind() {
        assert_eq!(TabWidth::NAME, "editor.tab-width");
        assert_eq!(TabWidth::DOC, "How wide a tab is rendered.");
        assert_eq!(TabWidth::DEFAULT, "8");
        assert_eq!(TabWidth::KIND, OptionKind::Integer);
        // NAME defaults to the kebab-cased type name; KIND from the field type.
        assert_eq!(WrapLines::NAME, "wrap-lines");
        assert_eq!(WrapLines::KIND, OptionKind::Boolean);
    }

    #[test]
    fn parse_option_reads_typed_values_and_errors_typed() {
        assert_eq!(parse_option::<TabWidth>("7").unwrap(), 7_i64);
        assert!(parse_option::<WrapLines>("true").unwrap());
        // A malformed string is a typed error, never a panic.
        let err = parse_option::<TabWidth>("not-a-number").unwrap_err();
        assert!(format!("{err}").contains("parse failed"));
    }
}
