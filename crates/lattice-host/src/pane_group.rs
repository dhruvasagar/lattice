//! D.4.a (2026-05-29): pane-group substrate.
//!
//! See `docs/dev/architecture/pane-groups.md` for the full
//! design; capsule:
//!
//! - A [`PaneGroup`] is a set of `(pane, buffer)` pairs that
//!   scroll together under a pluggable [`RowMapper`].
//! - Membership is keyed on the pair, not the pane alone,
//!   so the binding suspends naturally when the user
//!   switches a pane's buffer away from its registered
//!   counterpart and resumes when they switch back.
//! - Propagation runs at the dispatch tail
//!   ([`crate::Editor::publish_render_state`]); active pane
//!   drives, others follow.
//!
//! This slice ships the trait + registry; consumers
//! (`HunkRowMapper` in D.4.b, filler-row provider in
//! D.4.c, `:diffsplit` wiring in D.4.d) land in later
//! slices.

use std::sync::Arc;

use lattice_core::BufferId;
use lattice_core::ui::pane::{PaneGroupId, PaneId};

/// D.4.a: the `(pane, buffer)` pair a membership is keyed
/// on. Propagation observes a pane's *currently-displayed*
/// buffer at dispatch tail and skips the member when it no
/// longer matches `buffer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneGroupMember {
    pub pane: PaneId,
    pub buffer: BufferId,
}

/// D.4.a: pluggable row-mapping function for a pane group.
///
/// Indices are positions in `PaneGroup::members` (stable
/// across pane re-ordering). Mappers consult their own
/// state to translate; identity returns its input.
pub trait RowMapper: Send + Sync {
    fn map_row(&self, from_member_idx: usize, to_member_idx: usize, row: u32) -> u32;
}

/// D.4.a: default mapper for `:set scrollbind` parity.
/// Maps every row to itself.
#[derive(Debug, Default)]
pub struct IdentityRowMapper;

impl RowMapper for IdentityRowMapper {
    fn map_row(&self, _from: usize, _to: usize, row: u32) -> u32 {
        row
    }
}

/// D.4.a: one scroll-binding group.
///
/// `members` carries `(pane, buffer)` pairs; `mapper`
/// translates rows. Owned by `Editor::pane_groups`;
/// subsystems mint via [`crate::dispatch::Editor::add_pane_group`]
/// and drop via [`crate::dispatch::Editor::drop_pane_group`].
pub struct PaneGroup {
    pub id: PaneGroupId,
    pub members: Vec<PaneGroupMember>,
    pub mapper: Arc<dyn RowMapper>,
}

impl std::fmt::Debug for PaneGroup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneGroup")
            .field("id", &self.id)
            .field("members", &self.members)
            .field("mapper", &"<dyn RowMapper>")
            .finish()
    }
}

impl PaneGroup {
    /// Construct a new group with a freshly-allocated id.
    /// Callers populate `members` + supply a mapper.
    pub fn new(members: Vec<PaneGroupMember>, mapper: Arc<dyn RowMapper>) -> Self {
        Self {
            id: PaneGroupId::next(),
            members,
            mapper,
        }
    }

    /// Find a member's index by its `(pane, buffer)` pair.
    /// Returns `None` when no membership matches the pair.
    pub fn index_of(&self, member: PaneGroupMember) -> Option<usize> {
        self.members.iter().position(|m| *m == member)
    }
}

/// D.4.a: an offset-row stub mapper used by the unit tests
/// to verify that the mapping path is actually plumbed
/// (identity alone wouldn't distinguish "mapper was called"
/// from "mapper was bypassed"). Lives in non-test code so
/// downstream slices can reuse it as a smoke-test mapper.
#[derive(Debug)]
pub struct OffsetRowMapper {
    pub offset: i32,
}

impl RowMapper for OffsetRowMapper {
    fn map_row(&self, _from: usize, _to: usize, row: u32) -> u32 {
        if self.offset >= 0 {
            row.saturating_add(self.offset as u32)
        } else {
            row.saturating_sub(self.offset.unsigned_abs())
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn mk_member(pane: u32, buffer: u32) -> PaneGroupMember {
        PaneGroupMember {
            pane: PaneId(pane),
            buffer: BufferId(buffer),
        }
    }

    #[test]
    fn identity_mapper_returns_input_row() {
        let m = IdentityRowMapper;
        assert_eq!(m.map_row(0, 1, 42), 42);
        assert_eq!(m.map_row(3, 0, 0), 0);
    }

    #[test]
    fn offset_mapper_shifts_positive_and_negative() {
        let pos = OffsetRowMapper { offset: 5 };
        assert_eq!(pos.map_row(0, 1, 10), 15);
        let neg = OffsetRowMapper { offset: -3 };
        assert_eq!(neg.map_row(0, 1, 10), 7);
        // saturating: negative offset past zero
        assert_eq!(neg.map_row(0, 1, 1), 0);
    }

    #[test]
    fn group_id_is_unique() {
        let a = PaneGroup::new(vec![], Arc::new(IdentityRowMapper));
        let b = PaneGroup::new(vec![], Arc::new(IdentityRowMapper));
        assert_ne!(a.id, b.id);
    }

    #[test]
    fn index_of_finds_membership_by_pair() {
        let group = PaneGroup::new(
            vec![mk_member(1, 100), mk_member(2, 200)],
            Arc::new(IdentityRowMapper),
        );
        assert_eq!(group.index_of(mk_member(1, 100)), Some(0));
        assert_eq!(group.index_of(mk_member(2, 200)), Some(1));
        // Pane matches but buffer doesn't — not a member.
        assert_eq!(group.index_of(mk_member(1, 999)), None);
        // Buffer matches but pane doesn't — not a member.
        assert_eq!(group.index_of(mk_member(9, 100)), None);
    }
}
