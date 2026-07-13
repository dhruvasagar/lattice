//! `lattice-plugin-sdk` — the guest-side ergonomic layer OVER the opaque plugin
//! event wire (PH7.8b.3). Compiled INTO plugins (any component-model language via
//! its own toolchain; Rust today), never into the host.
//!
//! ## What it is
//!
//! The plugin-host `emit-event` / `register-event` host-services (PH7.8b.2) carry
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
//!
//! ## WIT-agnostic by design (approach A)
//!
//! This crate touches **no** plugin-host bindings — it is pure serde + a derive.
//! The host calls stay plugin-side one-liners using the derived constants:
//!
//! ```ignore
//! // at register-events:
//! host_services::register_event(MyEvent::NAME, MyEvent::DOC);
//! // to emit:
//! host_services::emit_event(MyEvent::NAME, &my_event.encode());
//! // in on-event(name, payload):
//! if let Some(ev) = lattice_plugin_sdk::try_decode::<MyEvent>(name, payload) {
//!     let ev = ev?; // a real MyEvent
//! }
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

// So the derive's generated `::lattice_plugin_sdk::..` paths resolve inside this
// crate's own tests (the `lattice-config` / serde precedent for a crate that
// consumes its own derive).
extern crate self as lattice_plugin_sdk;

pub use lattice_plugin_sdk_derive::PluginEvent;

/// A plugin-defined event: a typed view over the opaque `emit-event` /
/// `on-event` wire (PH7.8b.2). Implement via `#[derive(PluginEvent)]` on a
/// serde-serializable struct; hand-implementing is possible but rarely needed.
///
/// `NAME` is the wire identifier (matched by subscribers, registered via
/// `register-event`); `DOC` is the human summary surfaced in `:describe-event`.
/// `encode` / `decode` round-trip the payload as MessagePack.
pub trait PluginEvent: Sized {
    /// The event's wire name — the identifier crossed to `emit-event` and
    /// matched by subscribers (e.g. `"git.hunks-changed"`).
    const NAME: &'static str;
    /// The human-facing doc (from the struct's `///` comment), shown by
    /// `:describe-event` once registered.
    const DOC: &'static str;

    /// Serialize to the opaque MessagePack payload `emit-event` carries.
    fn encode(&self) -> Vec<u8>;

    /// Deserialize from a payload received on `on-event`. A malformed / mistyped
    /// payload is a typed [`DecodeError`], never a panic.
    fn decode(bytes: &[u8]) -> Result<Self, DecodeError>;
}

/// A failed [`PluginEvent::decode`] — the payload was not valid MessagePack for
/// the target type (wrong event type, version skew, corruption). Carries the
/// underlying decoder message; opaque and stable (it hides the serde impl).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "plugin event decode failed: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// Subscriber-side helper: if `name` names event `E`, decode `payload` into it;
/// otherwise `None` (the event is for a different subscriber). Folds the
/// name-gate the guest would otherwise write by hand in `on-event` into one call:
///
/// ```ignore
/// match lattice_plugin_sdk::try_decode::<Indexed>(name, payload) {
///     Some(Ok(ev)) => react(ev),
///     Some(Err(e)) => log(e),   // our event, but a bad payload
///     None => {}                // not our event
/// }
/// ```
pub fn try_decode<E: PluginEvent>(name: &str, payload: &[u8]) -> Option<Result<E, DecodeError>> {
    (name == E::NAME).then(|| E::decode(payload))
}

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
    #![allow(clippy::unwrap_used, clippy::panic)]

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
}
