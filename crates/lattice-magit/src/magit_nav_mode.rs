//! `magit-nav-mode` — the magit chords that are safe in ANY buffer.
//!
//! ## Why this is split from `magit-core-mode`
//!
//! `magit-core-mode` claims bare letters — `i`, `C`, `D`, `S`, `U`, `q`,
//! `yr` — and its own doc explains what makes that legitimate: every major
//! it attaches to is a **read-only list**, so those letters shadow nothing
//! the user could otherwise be doing. `magit-commit-mode` is excluded by
//! name for exactly that reason; listing it made `i` open the .gitignore
//! prompt instead of entering Insert, and a commit message became
//! untypeable.
//!
//! That rule is enforced by `ActivationPolicy::Majors`, which is an
//! explicit list. It is NOT enforced for a mode that reaches
//! `magit-core-mode` through `Mode::implies` — and `magit-project-diff-mode`
//! did exactly that, so the editable cross-file diff inherited every one of
//! those letters. `i` did not insert.
//!
//! Rather than override them back one by one — a subtractive rule written
//! additively, whose override list would have to track `magit-core-mode`
//! forever — the four chords that are *actually* universal move here.
//! `magit-core-mode` implies this mode, so read-only views are unchanged;
//! an editable magit view implies THIS one and never sees the letters.
//!
//! The distinction is what each chord assumes, not which buffer wants it:
//! navigating sections and folding are meaningful wherever there are
//! sections; `i`/`C`/`D`/`S` assume nothing is editable.

use std::sync::{Arc, OnceLock};

use lattice_mode::{
    ActivationPolicy, CapabilitySet, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext,
    ModeId, ModeKind, keymap_entry,
};

pub struct MagitNavMode;

impl MagitNavMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("magit-nav-mode")
    }
}

fn nav_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "]]", doc: "Next section", cmd: "action:magit-next-section" },
            keymap_entry! { mode: Normal, chord: "[[", doc: "Previous section", cmd: "action:magit-prev-section" },
            keymap_entry! { mode: Normal, chord: "<Tab>", doc: "Toggle fold", cmd: "action:magit-toggle-fold" },
            keymap_entry! { mode: Normal, chord: "<S-Tab>", doc: "Cycle sections", cmd: "action:magit-cycle-sections" },
        ]
    })
}

impl Mode for MagitNavMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// Manual: reached by implication from `magit-core-mode` (read-only
    /// views) or declared directly (editable ones). Never auto-attached —
    /// `]]` in an ordinary buffer is not magit's.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Manual
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(nav_entries())
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

pub fn register_magit_nav_mode(registry: &mut lattice_mode::ModeRegistry) {
    registry
        .register(MagitNavMode)
        .expect("magit-nav-mode registers without conflict");
}

/// The handlers these chords fire live on `magit-core-mode`, which
/// registers them once at boot. Kept as an `Arc` re-export point so the
/// split does not duplicate handler bodies — the rule this whole mode
/// exists to honour.
pub(crate) fn _handlers_live_on_core() -> Option<Arc<()>> {
    None
}
