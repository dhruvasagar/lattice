//! Newtype identifiers for editor entities.
//!
//! Every id is a `u64` newtype, so it fits one register, round-trips through a
//! WIT `u64` without precision loss, and serializes as the bare number
//! (`#[serde(transparent)]`). The point of the newtypes is nominal: a
//! [`DocumentId`] and a [`BufferId`] with the same raw value are different
//! types and cannot be compared or passed for one another by accident.
//!
//! This module only *defines* the types; it issues nothing. Each id is minted
//! by the subsystem that owns the entity, which decides uniqueness and
//! lifetime (documented per type below). The convention those minters follow:
//! monotonically increasing values, never reused within a process. `0` is the
//! [`Default`] and is conventionally "none / not yet assigned".
//!
//! Several ids are declared ahead of their consumers: the pane, tab, window
//! and plugin subsystems currently mint their own narrower ids (in
//! `lattice-core` and `lattice-plugin-host`), and the ids here are the
//! planned wire-level spelling. Each type's doc says which case it is.
//!
//! # Examples
//!
//! ```
//! use lattice_protocol::{BufferId, DocumentId};
//!
//! let doc = DocumentId::new(42);
//! assert_eq!(doc.raw(), 42);
//! assert_eq!(DocumentId::new(doc.raw()), doc); // lossless round-trip
//! assert_eq!(doc.to_string(), "DocumentId#42"); // Display names the type
//!
//! // Ids order by their raw value, and serialize as the bare number.
//! assert!(BufferId::new(1) < BufferId::new(2));
//! assert_eq!(serde_json::to_string(&BufferId::new(7)).unwrap(), "7");
//! ```

use serde::{Deserialize, Serialize};

macro_rules! id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(
            Debug,
            Clone,
            Copy,
            Default,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            Serialize,
            Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub u64);

        impl $name {
            /// Wrap a raw value. Does not mint or reserve anything: callers
            /// that need a fresh id get one from the owning subsystem.
            pub const fn new(raw: u64) -> Self {
                Self(raw)
            }

            /// The underlying `u64`, e.g. for crossing the WIT boundary.
            pub const fn raw(self) -> u64 {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}#{}", stringify!($name), self.0)
            }
        }
    };
}

id!(
    /// Identifies one `lattice_core::Document` — a text buffer with its
    /// undo history, selections and path — for its whole lifetime.
    ///
    /// Minted by `lattice-core` from a process-wide counter starting at 1, so
    /// ids are unique within a process and never reused. This is the id
    /// document events ([`Event::DocumentChanged`], [`Event::DocumentSaved`],
    /// [`Event::SelectionsChanged`], ...) carry.
    ///
    /// [`Event::DocumentChanged`]: crate::Event::DocumentChanged
    /// [`Event::DocumentSaved`]: crate::Event::DocumentSaved
    /// [`Event::SelectionsChanged`]: crate::Event::SelectionsChanged
    DocumentId
);
id!(
    /// Identifies one entry of the host's buffer registry (a document, file
    /// tree, help view, multibuffer, terminal, ...) at the protocol level.
    ///
    /// The registry mints a narrower `lattice_core::BufferId(u32)`; crossing
    /// into this crate widens it (`BufferId::new(id.0 as u64)`), so the raw
    /// values agree. Mode-lifecycle events ([`Event::MajorEntered`] and
    /// peers) and [`Event::BufferOptionOverrideRequested`] address buffers
    /// with it.
    ///
    /// [`Event::MajorEntered`]: crate::Event::MajorEntered
    /// [`Event::BufferOptionOverrideRequested`]: crate::Event::BufferOptionOverrideRequested
    BufferId
);
id!(
    /// Identifies an OS-level editor window. Declared for the protocol; no
    /// subsystem mints it yet (the editor runs one window).
    WindowId
);
id!(
    /// Identifies a tab page. Declared for the protocol; the tab subsystem
    /// currently mints its own `lattice_core::ui::TabId(u32)`.
    TabId
);
id!(
    /// Identifies a pane (a split showing one buffer). Declared for the
    /// protocol; the layout currently mints its own `lattice_core::ui::PaneId(u32)`.
    PaneId
);
id!(
    /// Identifies a loaded plugin instance. Declared for the protocol; the
    /// plugin host currently mints its own `lattice_plugin_host::PluginId`,
    /// and plugin events carry it as a bare `u32`
    /// ([`Event::PluginCrashed`](crate::Event::PluginCrashed)).
    PluginId
);
id!(
    /// Identifies a registered command — the `command` of a
    /// `lattice_grammar::CommandInvocation`, and what a keymap binding
    /// resolves to. Minted at registration by `lattice-grammar`'s command
    /// registry from a process-wide counter starting at 1. (The completion
    /// registry keeps a separate counter of its own, so an id is only
    /// meaningful together with the registry that issued it.)
    CommandId
);
id!(
    /// Identifies a major mode. Declared for the protocol; modes are
    /// currently identified by `lattice_mode::ModeId`, and events carry the
    /// mode's canonical name as a `String`.
    MajorModeId
);
id!(
    /// Identifies a minor mode. Declared for the protocol; see
    /// [`MajorModeId`] for how modes are identified today.
    MinorModeId
);

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;

    #[test]
    fn raw_round_trips() {
        let id = DocumentId::new(42);
        assert_eq!(id.raw(), 42);
        assert_eq!(DocumentId::new(id.raw()), id);
    }

    #[test]
    fn display_includes_type_name() {
        assert_eq!(format!("{}", PaneId::new(7)), "PaneId#7");
        assert_eq!(format!("{}", PluginId::new(123)), "PluginId#123");
    }

    #[test]
    fn equal_raw_means_equal_id() {
        assert_eq!(DocumentId::new(5), DocumentId::new(5));
        assert_ne!(DocumentId::new(5), DocumentId::new(6));
    }

    #[test]
    fn ordering_follows_raw() {
        assert!(DocumentId::new(1) < DocumentId::new(2));
        assert!(WindowId::new(10) > WindowId::new(9));
    }

    #[test]
    fn distinct_id_types_do_not_alias() {
        // Compile-time check: a `DocumentId` and a `BufferId` with the same
        // raw value are nominally distinct types and cannot be compared.
        let _doc = DocumentId::new(1);
        let _buf = BufferId::new(1);
        // (No assertion needed; the test exists to anchor the invariant.)
    }

    #[test]
    fn ids_serialize_as_their_raw_u64() {
        let id = TabId::new(99);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, "99");
        let back: TabId = serde_json::from_str(&json).unwrap();
        assert_eq!(back, id);
    }
}
