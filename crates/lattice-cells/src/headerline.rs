//! Generic sticky headerline — the one mechanism for a buffer to surface a
//! status row pinned above line 0.
//!
//! ## Roles
//!
//! - [`Headerline`] — the trait.  Any type that knows how to produce a row of
//!   cells (and a version counter) implements it.  Tutor, multibuffer search,
//!   LSP status, VCS branch, diagnostics summary — all use this same surface.
//!
//! - [`SimpleHeaderline`] / [`SimpleHeaderlineHandle`] — the ready-made
//!   implementation for modes that want **owned dedicated state**.  The handle
//!   is cheap-clone, updates via a closure, and bumps the version atomically.
//!   Modes that already carry their state elsewhere (e.g. an LSP session
//!   struct) implement [`Headerline`] directly.
//!
//! - [`HeaderlineProvider`] — wraps any [`Headerline`] impl and registers it
//!   as a [`VirtualRowProvider`].  Always emits one [`VirtualRowKind::Sticky`]
//!   row anchored above line 0; returns an empty vec when the impl returns
//!   `None` (hide the row).
//!
//! ## Design anchor
//!
//! `docs/dev/architecture/headerline.md`

use std::sync::{Arc, RwLock};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::cell::Cell;
use crate::virtual_rows::{
    AnchorPosition, ProviderId, VirtualRow, VirtualRowKind, VirtualRowProvider,
};

// ── Output type ──────────────────────────────────────────────────────────────

/// The row produced by a [`Headerline`] impl when it wants to be visible.
pub struct HeaderlineRow {
    /// Cells to paint.  Non-empty (callers return `None` when the row should
    /// be hidden instead of returning an empty cell slice).
    pub cells: Arc<[Cell]>,
    /// Override the renderer's sticky-row background.  `None` → renderer uses
    /// the theme-defined header background.  `Some(0xRRGGBB)` → hard-coded
    /// colour (e.g. tutor's retro palette).
    pub bg: Option<u32>,
}

// ── Trait ─────────────────────────────────────────────────────────────────────

/// Anything that can supply a sticky headerline row.
///
/// The cells worker calls `version()` on every tick.  When the version has
/// advanced, it calls `render()` to rebuild the displayed row.  `render()`
/// returns `None` to hide the row entirely (e.g. while idle).
pub trait Headerline: Send + Sync + 'static {
    /// Monotonic counter.  Bump whenever the row content changes.  The worker
    /// skips `render()` when the version is unchanged since the last call.
    fn version(&self) -> u64;

    /// Build the current row.  Return `None` to hide the header entirely.
    /// Must not block — cache results; background tasks push updates via the
    /// owning handle.
    fn render(&self) -> Option<HeaderlineRow>;
}

// ── SimpleHeaderline — owned-state convenience ───────────────────────────────

/// Ready-made [`Headerline`] impl for modes with dedicated header state.
///
/// Not constructed directly — create via [`SimpleHeaderlineHandle::new`].
pub struct SimpleHeaderline<S: Send + Sync + 'static> {
    state: Arc<RwLock<S>>,
    version: AtomicU64,
    renderer: Arc<dyn Fn(&S) -> Option<HeaderlineRow> + Send + Sync>,
}

impl<S: Send + Sync + 'static> std::fmt::Debug for SimpleHeaderline<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SimpleHeaderline")
            .field("version", &self.version.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl<S: Send + Sync + 'static> Headerline for SimpleHeaderline<S> {
    fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    fn render(&self) -> Option<HeaderlineRow> {
        self.state.read().ok().and_then(|s| (self.renderer)(&s))
    }
}

// ── SimpleHeaderlineHandle ────────────────────────────────────────────────────

/// Cheap-clone handle to a [`SimpleHeaderline<S>`].
///
/// The mode holds the handle; the paired [`HeaderlineProvider`] holds a type-
/// erased `Arc<dyn Headerline>` pointing to the same allocation.  Updates via
/// [`update`] are immediately visible to the next `render()` call.
///
/// [`update`]: SimpleHeaderlineHandle::update
pub struct SimpleHeaderlineHandle<S: Send + Sync + 'static>(Arc<SimpleHeaderline<S>>);

// Manual impl so S does not need to be Clone — we only clone the Arc.
impl<S: Send + Sync + 'static> Clone for SimpleHeaderlineHandle<S> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<S: Send + Sync + 'static> std::fmt::Debug for SimpleHeaderlineHandle<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SimpleHeaderlineHandle(v={})", self.version())
    }
}

impl<S: Send + Sync + 'static> SimpleHeaderlineHandle<S> {
    /// Create a new handle with `initial` state and a `renderer` closure.
    ///
    /// The closure receives a shared reference to the state and returns the
    /// row to display, or `None` to hide the header.
    pub fn new(
        initial: S,
        renderer: impl Fn(&S) -> Option<HeaderlineRow> + Send + Sync + 'static,
    ) -> Self {
        Self(Arc::new(SimpleHeaderline {
            state: Arc::new(RwLock::new(initial)),
            version: AtomicU64::new(0),
            renderer: Arc::new(renderer),
        }))
    }

    /// Mutate the state and bump the version so the cells worker rebuilds the
    /// row on the next tick.
    pub fn update(&self, f: impl FnOnce(&mut S)) {
        if let Ok(mut s) = self.0.state.write() {
            f(&mut s);
        }
        self.0.version.fetch_add(1, Ordering::Release);
    }

    /// Current version — useful for diagnostics / `BufferLocal::describe`.
    pub fn version(&self) -> u64 {
        self.0.version.load(Ordering::Acquire)
    }

    /// Construct a [`HeaderlineProvider`] backed by this handle.  Register the
    /// result with `register_virtual_row_provider`; keep the handle for updates.
    pub fn provider(&self, provider_id: ProviderId) -> HeaderlineProvider {
        HeaderlineProvider {
            provider_id,
            inner: Arc::clone(&self.0) as Arc<dyn Headerline>,
        }
    }
}

// ── HeaderlineProvider ────────────────────────────────────────────────────────

/// [`VirtualRowProvider`] that emits one sticky row above line 0 from any
/// [`Headerline`] impl.
///
/// Register this the same way as any other provider.  The row is hidden
/// (empty `collect()` result) when the impl returns `None` from `render()`.
pub struct HeaderlineProvider {
    provider_id: ProviderId,
    inner: Arc<dyn Headerline>,
}

impl std::fmt::Debug for HeaderlineProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HeaderlineProvider")
            .field("provider_id", &self.provider_id)
            .field("version", &self.inner.version())
            .finish()
    }
}

impl HeaderlineProvider {
    /// Wrap any [`Headerline`] impl directly (e.g. when the mode implements
    /// the trait on its own existing state struct).
    pub fn new(provider_id: ProviderId, inner: Arc<dyn Headerline>) -> Self {
        Self { provider_id, inner }
    }
}

impl VirtualRowProvider for HeaderlineProvider {
    fn id(&self) -> ProviderId {
        self.provider_id
    }

    fn version(&self) -> u64 {
        self.inner.version()
    }

    fn collect(&self) -> Vec<VirtualRow> {
        let Some(row) = self.inner.render() else {
            return Vec::new();
        };
        vec![VirtualRow {
            anchor_line: 0,
            position: AnchorPosition::Above,
            cells: row.cells,
            height: 1,
            kind: VirtualRowKind::Sticky,
            bg: row.bg,
            scales: None,
        }]
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hidden_when_renderer_returns_none() {
        let handle = SimpleHeaderlineHandle::new(0u32, |_| None);
        let provider = handle.provider(1);
        assert!(provider.collect().is_empty());
    }

    #[test]
    fn emits_sticky_row_at_line_zero() {
        let handle = SimpleHeaderlineHandle::<()>::new((), |_| {
            let cells: Arc<[Cell]> = vec![Cell::new('x' as u32, 0xffffff, 0, 0)].into();
            Some(HeaderlineRow { cells, bg: Some(0x000000) })
        });
        let provider = handle.provider(42);
        let rows = provider.collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].anchor_line, 0);
        assert_eq!(rows[0].kind, VirtualRowKind::Sticky);
        assert_eq!(rows[0].bg, Some(0x000000));
    }

    #[test]
    fn version_advances_on_update() {
        let handle = SimpleHeaderlineHandle::new(0u32, |_| None);
        let v0 = handle.version();
        handle.update(|s| *s = 1);
        assert!(handle.version() > v0);
    }

    #[test]
    fn provider_version_tracks_handle() {
        let handle = SimpleHeaderlineHandle::new(0u32, |_| None);
        let provider = handle.provider(99);
        let v0 = provider.version();
        handle.update(|s| *s += 1);
        assert!(provider.version() > v0);
    }

    #[test]
    fn direct_headerline_impl_works() {
        struct Fixed;
        impl Headerline for Fixed {
            fn version(&self) -> u64 { 1 }
            fn render(&self) -> Option<HeaderlineRow> {
                let cells: Arc<[Cell]> = vec![Cell::new('!' as u32, 0, 0, 0)].into();
                Some(HeaderlineRow { cells, bg: None })
            }
        }
        let p = HeaderlineProvider::new(7, Arc::new(Fixed));
        assert_eq!(p.collect().len(), 1);
        assert_eq!(p.collect()[0].bg, None);
    }
}
