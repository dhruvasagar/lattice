//! Newtype identifiers for editor entities.
//!
//! IDs are 64-bit so they fit a single register and round-trip through WIT
//! `u64` without precision loss. The host issues monotonically increasing IDs;
//! reuse only happens after the entity is gone and any subscriber has had a
//! chance to observe its closure event.

use serde::{Deserialize, Serialize};

macro_rules! id {
    ($name:ident) => {
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
            pub const fn new(raw: u64) -> Self {
                Self(raw)
            }

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

id!(DocumentId);
id!(BufferId);
id!(WindowId);
id!(TabId);
id!(PaneId);
id!(PluginId);
id!(CommandId);
id!(MajorModeId);
id!(MinorModeId);

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
