//! IM.4 — resolving an inline media block's pixels, off the UI thread.
//!
//! Design: `docs/dev/architecture/inline-media.md` §5.
//!
//! ## The rule this crate exists to enforce
//!
//! **Decode never happens on the UI thread.** `gpui::img()` will happily take
//! a path and load it, which means file I/O and PNG decode inside the render
//! pass — precisely the forbidden pattern (paramount goal #1). So the peer
//! never gets a path to draw; it gets pixels that were produced elsewhere, or
//! it gets nothing and paints the placeholder.
//!
//! The guarantee is structural rather than a convention: this crate depends on
//! neither the host nor a renderer, so nothing here *can* reach a paint path.
//!
//! ## Size before pixels
//!
//! [`probe`] reads only enough of the file to learn its dimensions;
//! [`decode`] reads and decodes the whole thing. They are separate because a
//! block must be able to reserve the right amount of space **before** its
//! pixels exist — otherwise the document reflows when an image finishes
//! loading, which breaks the keystroke contract ("no pixel change to content
//! the user did not edit") on every scroll past it.
//!
//! A header read is bounded and cheap; it still happens off-thread, because
//! "cheap" and "on the UI thread" are different claims.
//!
//! ## The cache
//!
//! Keyed by `(path, mtime, target)`. `mtime` is what makes an edited image
//! reappear rather than serving a stale decode forever, and `target` is in the
//! key because the same file at two display scales is two different decodes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Decoded, scaled pixels ready for a peer to upload.
#[derive(Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    /// RGBA8, row-major, `width * height * 4` bytes.
    pub rgba: Arc<[u8]>,
}

impl std::fmt::Debug for DecodedImage {
    /// Hand-written so a failing assertion prints dimensions instead of
    /// several megabytes of pixels.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .finish()
    }
}

/// Why a block has no pixels. Every variant is a *placeholder plus alt text*
/// outcome, never a panic and never a stall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaError {
    /// The file is not there, or is not readable.
    Unreadable(String),
    /// Read fine, but no decoder recognised it.
    Undecodable(String),
    /// Bigger than [`MAX_PIXELS`]. Refused before allocating, so a
    /// pathological or hostile image cannot exhaust memory.
    TooLarge { width: u32, height: u32 },
}

impl std::fmt::Display for MediaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreadable(e) => write!(f, "unreadable: {e}"),
            Self::Undecodable(e) => write!(f, "undecodable: {e}"),
            Self::TooLarge { width, height } => {
                write!(f, "too large: {width}x{height}")
            }
        }
    }
}

/// Refuse anything above ~64 megapixels.
///
/// Not a performance tuning knob — a decoded 64MP image is a quarter of a
/// gigabyte of RGBA, and the dimensions come from a file header that a
/// document can reference without the user having looked at it. Checking
/// before allocating turns "the editor died opening a note" into "that image
/// shows its alt text".
pub const MAX_PIXELS: u64 = 64 * 1024 * 1024;

/// A file's natural size, from its header alone.
pub fn probe(path: &Path) -> Result<(u32, u32), MediaError> {
    let reader = image::ImageReader::open(path)
        .map_err(|e| MediaError::Unreadable(e.to_string()))?
        .with_guessed_format()
        .map_err(|e| MediaError::Unreadable(e.to_string()))?;
    let (w, h) = reader
        .into_dimensions()
        .map_err(|e| MediaError::Undecodable(e.to_string()))?;
    guard_size(w, h)?;
    Ok((w, h))
}

/// Decode `path` and scale it to fit `target` (width, height in px),
/// preserving aspect ratio and never scaling up.
///
/// Never upscales: a 32×32 icon blown up to fill a block is worse than the
/// same icon shown small, and the caller cannot tell the difference from the
/// returned dimensions alone.
pub fn decode(path: &Path, target: (u32, u32)) -> Result<DecodedImage, MediaError> {
    let (w, h) = probe(path)?;
    let img = image::ImageReader::open(path)
        .map_err(|e| MediaError::Unreadable(e.to_string()))?
        .with_guessed_format()
        .map_err(|e| MediaError::Unreadable(e.to_string()))?
        .decode()
        .map_err(|e| MediaError::Undecodable(e.to_string()))?;

    let (tw, th) = fit_within((w, h), target);
    let scaled = if (tw, th) == (w, h) {
        img
    } else {
        img.resize(tw, th, image::imageops::FilterType::Triangle)
    };
    let rgba = scaled.to_rgba8();
    Ok(DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: Arc::from(rgba.into_raw().into_boxed_slice()),
    })
}

fn guard_size(w: u32, h: u32) -> Result<(), MediaError> {
    if w == 0 || h == 0 {
        return Err(MediaError::Undecodable("zero-sized image".into()));
    }
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return Err(MediaError::TooLarge {
            width: w,
            height: h,
        });
    }
    Ok(())
}

/// Scale `src` down to fit inside `bounds`, preserving aspect ratio. Returns
/// `src` unchanged when it already fits — the never-upscale rule.
pub fn fit_within(src: (u32, u32), bounds: (u32, u32)) -> (u32, u32) {
    let (sw, sh) = src;
    let (bw, bh) = bounds;
    if bw == 0 || bh == 0 || (sw <= bw && sh <= bh) {
        return src;
    }
    let scale = (bw as f64 / sw as f64).min(bh as f64 / sh as f64);
    // `max(1)`: a very wide, very short image must not scale to zero rows and
    // vanish. One pixel is honest; zero is a disappearance.
    (
        ((sw as f64 * scale).round() as u32).max(1),
        ((sh as f64 * scale).round() as u32).max(1),
    )
}

/// What a cache entry is keyed on.
///
/// `mtime` is why an edited image reappears instead of serving a stale decode
/// forever; `target` is in the key because the same file at two display scales
/// is genuinely two decodes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CacheKey {
    path: PathBuf,
    mtime: Option<i64>,
    target: (u32, u32),
}

/// A bounded decode cache.
///
/// Bounded by total decoded bytes rather than entry count: entries differ in
/// size by orders of magnitude, so counting them bounds nothing that matters.
/// Eviction is oldest-inserted-first, which is the right default for a
/// document being read top to bottom.
pub struct MediaCache {
    inner: Mutex<CacheInner>,
    budget_bytes: usize,
}

struct CacheInner {
    entries: HashMap<CacheKey, Arc<DecodedImage>>,
    order: Vec<CacheKey>,
    bytes: usize,
}

impl MediaCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            inner: Mutex::new(CacheInner {
                entries: HashMap::new(),
                order: Vec::new(),
                bytes: 0,
            }),
            budget_bytes,
        }
    }

    /// Decoded pixels for `path` at `target`, decoding on a blocking thread if
    /// they are not cached.
    ///
    /// `spawn_blocking`, not `spawn`: the editor actor runs on a
    /// `current_thread` runtime, so a plain `spawn` would land the decode on
    /// the actor thread and stall exactly what this crate exists to protect.
    pub async fn get(
        self: &Arc<Self>,
        path: &Path,
        target: (u32, u32),
    ) -> Result<Arc<DecodedImage>, MediaError> {
        let key = CacheKey {
            path: path.to_path_buf(),
            mtime: mtime_of(path),
            target,
        };
        if let Some(hit) = self.lookup(&key) {
            return Ok(hit);
        }
        let owned = path.to_path_buf();
        let decoded = tokio::task::spawn_blocking(move || decode(&owned, target))
            .await
            .map_err(|e| MediaError::Unreadable(format!("decode task failed: {e}")))??;
        let decoded = Arc::new(decoded);
        self.insert(key, decoded.clone());
        Ok(decoded)
    }

    fn lookup(&self, key: &CacheKey) -> Option<Arc<DecodedImage>> {
        self.inner.lock().ok()?.entries.get(key).cloned()
    }

    fn insert(&self, key: CacheKey, value: Arc<DecodedImage>) {
        let Ok(mut inner) = self.inner.lock() else {
            // A poisoned cache mutex must not take the editor with it: skip
            // the caching and let the next request decode again.
            tracing::warn!("media cache mutex poisoned; skipping insert");
            return;
        };
        let size = value.rgba.len();
        if inner.entries.insert(key.clone(), value).is_none() {
            inner.order.push(key);
            inner.bytes += size;
        }
        while inner.bytes > self.budget_bytes && !inner.order.is_empty() {
            let oldest = inner.order.remove(0);
            if let Some(dropped) = inner.entries.remove(&oldest) {
                inner.bytes = inner.bytes.saturating_sub(dropped.rgba.len());
            }
        }
    }

    /// Current cached byte total. Diagnostics and tests.
    pub fn bytes(&self) -> usize {
        self.inner.lock().map(|i| i.bytes).unwrap_or(0)
    }

    /// Drop everything — called when the last buffer referencing media closes.
    pub fn clear(&self) {
        if let Ok(mut inner) = self.inner.lock() {
            inner.entries.clear();
            inner.order.clear();
            inner.bytes = 0;
        }
    }
}

fn mtime_of(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let dur = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_png(dir: &Path, name: &str, w: u32, h: u32) -> PathBuf {
        let path = dir.join(name);
        let buf = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        buf.save(&path).expect("write png");
        path
    }

    #[test]
    fn probe_reads_dimensions_without_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_png(dir.path(), "a.png", 40, 25);
        assert_eq!(probe(&p).unwrap(), (40, 25));
    }

    /// Every failure is a placeholder outcome, never a panic. A document can
    /// reference a file the user has never looked at.
    #[test]
    fn every_failure_mode_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            probe(&dir.path().join("nope.png")),
            Err(MediaError::Unreadable(_))
        ));

        let junk = dir.path().join("junk.png");
        std::fs::write(&junk, b"this is not a png").unwrap();
        assert!(probe(&junk).is_err(), "garbage is refused, not decoded");
    }

    #[test]
    fn fit_never_upscales_and_never_vanishes() {
        // Already fits ⇒ untouched.
        assert_eq!(fit_within((40, 25), (100, 100)), (40, 25));
        // Scales down preserving ratio.
        assert_eq!(fit_within((200, 100), (100, 100)), (100, 50));
        assert_eq!(fit_within((100, 200), (100, 100)), (50, 100));
        // A very wide, very short image must not scale to zero rows and
        // disappear — one pixel is honest, zero is a vanishing.
        let (w, h) = fit_within((10_000, 3), (100, 100));
        assert!(h >= 1 && w >= 1, "got {w}x{h}");
    }

    /// The dimensions come from a file header a document can reference
    /// without the user ever looking at it, so the refusal happens BEFORE the
    /// allocation.
    #[test]
    fn an_absurd_size_is_refused_before_allocating() {
        assert!(matches!(
            guard_size(100_000, 100_000),
            Err(MediaError::TooLarge { .. })
        ));
        assert!(guard_size(0, 10).is_err(), "zero-sized is not an image");
        assert!(guard_size(1920, 1080).is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn decoding_scales_to_fit_and_caches_the_result() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_png(dir.path(), "big.png", 200, 100);
        let cache = Arc::new(MediaCache::new(8 * 1024 * 1024));

        let first = cache.get(&p, (100, 100)).await.expect("decodes");
        assert_eq!((first.width, first.height), (100, 50), "scaled to fit");
        assert_eq!(first.rgba.len(), 100 * 50 * 4, "RGBA8");

        let again = cache.get(&p, (100, 100)).await.expect("decodes");
        assert!(Arc::ptr_eq(&first, &again), "second get is a cache hit");

        // A different target is a different decode, not a stale hit.
        let other = cache.get(&p, (40, 40)).await.expect("decodes");
        assert_eq!((other.width, other.height), (40, 20));
    }

    /// The cache is bounded by BYTES, not entries: decoded images differ in
    /// size by orders of magnitude, so an entry count bounds nothing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_cache_evicts_to_stay_within_its_byte_budget() {
        let dir = tempfile::tempdir().unwrap();
        // 100x100 RGBA = 40_000 bytes each; budget holds one.
        let cache = Arc::new(MediaCache::new(50_000));
        for i in 0..4 {
            let p = write_png(dir.path(), &format!("{i}.png"), 100, 100);
            cache.get(&p, (100, 100)).await.expect("decodes");
            assert!(
                cache.bytes() <= 50_000,
                "over budget after {i}: {}",
                cache.bytes()
            );
        }
        cache.clear();
        assert_eq!(cache.bytes(), 0);
    }

    /// An edited image must reappear rather than serving the old decode
    /// forever — which is what `mtime` is in the key for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rewriting_the_file_invalidates_its_entry() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_png(dir.path(), "x.png", 40, 40);
        let cache = Arc::new(MediaCache::new(8 * 1024 * 1024));
        let first = cache.get(&p, (100, 100)).await.unwrap();
        assert_eq!((first.width, first.height), (40, 40));

        // Rewrite at a different size, with an mtime the filesystem will
        // report as newer.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write_png(dir.path(), "x.png", 20, 20);

        let second = cache.get(&p, (100, 100)).await.unwrap();
        assert_eq!(
            (second.width, second.height),
            (20, 20),
            "the rewritten file was decoded again, not served from cache"
        );
    }
}
