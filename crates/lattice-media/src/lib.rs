//! IM.4 — resolving an inline media block's pixels, off the UI thread.
//!
//! Design: `docs/dev/architecture/inline-media.md` §5.
//!
//! ## The rule this code exists to serve
//!
//! **Decode never happens on the UI thread.** `gpui::img()` will happily take
//! a path and load it, which means file I/O and PNG decode inside the render
//! pass — precisely the forbidden pattern (paramount goal #1). So the peer
//! never gets a path to draw; it gets pixels that were produced elsewhere, or
//! it gets nothing and paints the placeholder.
//!
//! That guarantee lives at the CALL SITE, not here. [`MediaCache::get`] is
//! async and goes through `spawn_blocking`, which is the ergonomic path; but
//! [`decode`] is public and synchronous, and nothing in this crate's
//! dependency graph prevents a renderer calling it from inside a paint. The
//! rule is enforced by the caller and by the tests that assert frame time is
//! unaffected — stating otherwise would be a false comfort.
//!
//! ## Why a crate, and why not inside the GPUI peer
//!
//! Only GPUI draws images today. This is not in `lattice-ui-gpui` because
//! terminal graphics (kitty / sixel / iTerm2) is deliberately not foreclosed —
//! see `docs/dev/architecture/inline-media.md` §9 — and the day the TUI grows
//! an image path, both peers need this code. A thing both peers need can live
//! in neither of them.
//!
//! It is not in `lattice-cells` either: that crate has one dependency and ten
//! dependents, and none of `lattice-completion`, `lattice-listing` or
//! `lattice-diff` should be compiling PNG decoders.
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

/// The byte layout a consumer needs.
///
/// A parameter rather than a fixed output, and that is not speculative
/// generality — GPUI's `RenderImage` is **premultiplied BGRA** while a
/// terminal graphics protocol wants straight RGBA. Converting at the
/// consumer would put a per-pixel loop in the paint path, which is the exact
/// thing this crate exists to keep out of it, so the swap happens inside the
/// same off-thread decode and is cached in that form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PixelFormat {
    /// Straight RGBA8. What image files decode to, and what a kitty / sixel
    /// path would want.
    #[default]
    Rgba8,
    /// Premultiplied BGRA8 — `gpui::RenderImage`'s required layout.
    BgraPremultiplied8,
}

/// Decoded, scaled pixels ready for a peer to upload.
#[derive(Clone, PartialEq, Eq)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Row-major, `width * height * 4` bytes, in [`Self::format`].
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

/// True when `path` is an SVG, which is a document rather than a raster and
/// takes an entirely different route through this crate.
fn is_svg(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("svg"))
}

/// System fonts, loaded at most once.
///
/// SVG text is converted to paths at PARSE time, so a tree built without
/// fonts silently drops every `<text>` element — a diagram renders with its
/// boxes and none of its labels, which looks like a rendering bug rather than
/// a missing font. Loading is slow enough (tens to hundreds of milliseconds,
/// walking the system font directories) that it must happen once, and it is
/// only ever reached from `decode`, which the callers run on a blocking
/// thread.
///
/// `probe` deliberately does NOT take this path: a document's size comes from
/// its root element, so measuring needs no fonts and must not pay for them.
fn system_fonts() -> std::sync::Arc<resvg::usvg::fontdb::Database> {
    static FONTS: std::sync::OnceLock<std::sync::Arc<resvg::usvg::fontdb::Database>> =
        std::sync::OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = resvg::usvg::fontdb::Database::new();
            db.load_system_fonts();
            std::sync::Arc::new(db)
        })
        .clone()
}

/// Parse an SVG into a `usvg` tree.
///
/// `with_fonts` decides whether `<text>` survives — see [`system_fonts`].
/// `resources_dir` is the file's own directory, so an SVG that references a
/// bitmap or a stylesheet beside it resolves relative to ITSELF rather than
/// to wherever the editor was launched — the same rule the host applies to an
/// org link, for the same reason.
fn svg_tree(path: &Path, with_fonts: bool) -> Result<resvg::usvg::Tree, MediaError> {
    let data = std::fs::read(path).map_err(|e| MediaError::Unreadable(e.to_string()))?;
    let mut opt = resvg::usvg::Options {
        resources_dir: path.parent().map(std::path::Path::to_path_buf),
        ..Default::default()
    };
    if with_fonts {
        opt.fontdb = system_fonts();
    }
    resvg::usvg::Tree::from_data(&data, &opt).map_err(|e| MediaError::Undecodable(e.to_string()))
}

/// An SVG's natural size, rounded UP to whole pixels.
///
/// Up, not nearest: a 24.2-pixel-wide drawing needs 25 pixels to hold it, and
/// rounding down would clip a column.
fn svg_size(tree: &resvg::usvg::Tree) -> (u32, u32) {
    let size = tree.size();
    (
        size.width().ceil().max(1.0) as u32,
        size.height().ceil().max(1.0) as u32,
    )
}

/// A file's natural size, from its header alone.
pub fn probe(path: &Path) -> Result<(u32, u32), MediaError> {
    if is_svg(path) {
        let (w, h) = svg_size(&svg_tree(path, /* with_fonts */ false)?);
        guard_size(w, h)?;
        return Ok((w, h));
    }
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
pub fn decode(
    path: &Path,
    target: (u32, u32),
    format: PixelFormat,
) -> Result<DecodedImage, MediaError> {
    if is_svg(path) {
        return decode_svg(path, target, format);
    }
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
    let (w, h) = (rgba.width(), rgba.height());
    let mut bytes = rgba.into_raw();
    if format == PixelFormat::BgraPremultiplied8 {
        premultiply_to_bgra(&mut bytes);
    }
    Ok(DecodedImage {
        width: w,
        height: h,
        format,
        rgba: Arc::from(bytes.into_boxed_slice()),
    })
}

/// Rasterise an SVG straight to the size it will be drawn at.
///
/// The one place in this crate where scaling is not a loss. A raster is
/// decoded at its natural size and resampled down; a vector is *rendered* at
/// the target, so a diagram shown at 300px is as crisp as the same diagram
/// shown at 3000. That is also why the render transform is built from the
/// fitted size rather than the pixmap being resized afterwards.
///
/// [`fit_within`] still applies, so the never-upscale rule holds: a 24×24
/// icon stays 24×24 rather than being blown across the pane. Faithful, and
/// consistent with what the raster path does with the same picture.
fn decode_svg(
    path: &Path,
    target: (u32, u32),
    format: PixelFormat,
) -> Result<DecodedImage, MediaError> {
    let tree = svg_tree(path, /* with_fonts */ true)?;
    let natural = svg_size(&tree);
    guard_size(natural.0, natural.1)?;
    let (tw, th) = fit_within(natural, target);
    let mut pixmap = resvg::tiny_skia::Pixmap::new(tw.max(1), th.max(1))
        .ok_or_else(|| MediaError::Undecodable(format!("cannot allocate a {tw}x{th} pixmap")))?;
    let size = tree.size();
    let transform = resvg::tiny_skia::Transform::from_scale(
        tw as f32 / size.width(),
        th as f32 / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());

    // tiny-skia hands back PREMULTIPLIED RGBA, which is not what either of
    // our formats is by default. The raster path premultiplies on the way to
    // BGRA; this one is already premultiplied and only needs the channel
    // swap — running `premultiply_to_bgra` here would multiply by alpha a
    // second time and darken every soft edge.
    let (w, h) = (pixmap.width(), pixmap.height());
    let mut bytes = pixmap.take();
    match format {
        PixelFormat::BgraPremultiplied8 => swap_rb(&mut bytes),
        PixelFormat::Rgba8 => unpremultiply(&mut bytes),
    }
    Ok(DecodedImage {
        width: w,
        height: h,
        format,
        rgba: Arc::from(bytes.into_boxed_slice()),
    })
}

/// In-place channel swap: premultiplied RGBA8 → premultiplied BGRA8.
///
/// No arithmetic — the alpha has already been applied by the rasteriser.
fn swap_rb(bytes: &mut [u8]) {
    for px in bytes.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
}

/// In-place premultiplied RGBA8 → straight RGBA8.
///
/// The inverse of what [`premultiply_to_bgra`] does to the colour channels,
/// for the peers that want straight alpha (a terminal graphics protocol).
/// `a == 0` keeps the pixel black rather than dividing by zero: a fully
/// transparent pixel has no colour to recover.
fn unpremultiply(bytes: &mut [u8]) {
    for px in bytes.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 || a == 255 {
            continue;
        }
        for c in &mut px[..3] {
            *c = ((*c as u16 * 255 + (a as u16 / 2)) / a as u16).min(255) as u8;
        }
    }
}

/// In-place RGBA8 → premultiplied BGRA8.
///
/// Runs inside the off-thread decode, never at paint. Premultiplication is
/// what stops a transparent PNG showing a dark halo where the compositor
/// blends un-premultiplied colour against the background.
fn premultiply_to_bgra(bytes: &mut [u8]) {
    for px in bytes.chunks_exact_mut(4) {
        let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
        // `+ 127) / 255` rounds instead of truncating; truncation darkens
        // every semi-transparent pixel by up to one level, which is visible
        // as a dingy edge on antialiased artwork.
        let mul = |c: u8| (((c as u16) * (a as u16) + 127) / 255) as u8;
        px[0] = mul(b);
        px[1] = mul(g);
        px[2] = mul(r);
        px[3] = a;
    }
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

/// How much space a block should take, given what it is and where it goes.
///
/// Returns `(rows, height_lh)`:
///
/// - `rows` — display rows to reserve. **Both peers agree on this**, which is
///   what keeps `scroll` (a row index) anchoring the same source line
///   whichever renderer is running.
/// - `height_lh` — the drawn height in line-heights, which the drawing peer
///   spends through `RowWeights`. Deliberately allowed to be fractional: a
///   3.4-line-height image reserves 4 rows and draws 3.4 of them. Snapping it
///   to 4 is what §3 of the design rejected, because it makes the rendered
///   size a function of the font size.
///
/// `rows` is `ceil(height_lh)` so the reservation never under-covers the
/// drawing — an image must not paint over the line beneath it.
pub fn block_geometry(
    intrinsic: (u32, u32),
    fit: lattice_cells::MediaFit,
    line_height_px: f32,
    pane_width_px: f32,
) -> (u16, f32) {
    // Degenerate geometry (a pane not yet measured, a zero line height)
    // yields one row rather than a division by zero or an absurd
    // reservation. The next frame with real metrics resolves it properly.
    // `is_finite` first: a NaN pane width would slip past a bare `<= 0.0`
    // comparison and propagate into the row count.
    if !line_height_px.is_finite()
        || !pane_width_px.is_finite()
        || line_height_px <= 0.0
        || pane_width_px <= 0.0
    {
        return (1, 1.0);
    }
    let (iw, ih) = intrinsic;
    if iw == 0 || ih == 0 {
        return (1, 1.0);
    }

    let drawn_h_px = match fit {
        // Never wider than the pane, and never scaled UP — a 32×32 icon
        // stretched across the pane is worse than the icon.
        lattice_cells::MediaFit::Contain => {
            let scale = (pane_width_px / iw as f32).min(1.0);
            ih as f32 * scale
        }
        // Always fill the width, up or down, and let the height follow.
        lattice_cells::MediaFit::Width => ih as f32 * (pane_width_px / iw as f32),
    };

    let height_lh = (drawn_h_px / line_height_px).max(MIN_BLOCK_LH);
    let rows = height_lh.ceil().min(u16::MAX as f32) as u16;
    (rows.max(1), height_lh)
}

/// A block never draws thinner than this, however extreme its aspect ratio.
///
/// A 10000×1 rule would otherwise resolve to a fraction of a line-height and
/// be invisible while still consuming a row — the "it silently did nothing"
/// failure that `media_block_rows` clamps against on the row side.
pub const MIN_BLOCK_LH: f32 = 1.0;

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
    format: PixelFormat,
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
        format: PixelFormat,
    ) -> Result<Arc<DecodedImage>, MediaError> {
        let key = CacheKey {
            path: path.to_path_buf(),
            mtime: mtime_of(path),
            target,
            format,
        };
        if let Some(hit) = self.lookup(&key) {
            return Ok(hit);
        }
        let owned = path.to_path_buf();
        let decoded = tokio::task::spawn_blocking(move || decode(&owned, target, format))
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

    fn write_svg(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).expect("write svg");
        path
    }

    /// A 40×20 drawing, opaque red, no text — so it renders identically with
    /// or without fonts.
    const RED_40X20: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="20">
        <rect x="0" y="0" width="40" height="20" fill="#ff0000"/>
    </svg>"##;

    /// An SVG is measured from its root element — no rasterising, and no
    /// fonts. Before this, `image` did not recognise the file at all and the
    /// block fell back to its alt text.
    #[test]
    fn probe_reads_an_svgs_declared_size() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_svg(dir.path(), "d.svg", RED_40X20);
        assert_eq!(probe(&path).unwrap(), (40, 20));
    }

    /// A fractional size needs the whole pixel that holds it; rounding down
    /// would clip a column.
    #[test]
    fn an_svgs_size_rounds_up_to_whole_pixels() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_svg(
            dir.path(),
            "f.svg",
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="10.2" height="4.1"></svg>"#,
        );
        assert_eq!(probe(&path).unwrap(), (11, 5));
    }

    /// A vector is RENDERED at the target rather than resampled to it, but
    /// the never-upscale rule still holds — so a small drawing in a large
    /// box stays its own size.
    #[test]
    fn an_svg_renders_at_the_fitted_size_and_is_never_upscaled() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_svg(dir.path(), "d.svg", RED_40X20);

        let small = decode(&path, (20, 10), PixelFormat::Rgba8).unwrap();
        assert_eq!((small.width, small.height), (20, 10), "scaled down to fit");

        let huge = decode(&path, (4000, 4000), PixelFormat::Rgba8).unwrap();
        assert_eq!(
            (huge.width, huge.height),
            (40, 20),
            "never upscaled, exactly as a raster of the same size is not"
        );
    }

    /// The colour has to survive the trip. tiny-skia returns PREMULTIPLIED
    /// RGBA, so running the raster path's `premultiply_to_bgra` over it would
    /// apply alpha twice; `Rgba8` has to undo the premultiplication instead.
    /// Both directions are checked against a known pixel, because either
    /// mistake shows up as "slightly wrong colours" rather than as a failure.
    #[test]
    fn an_opaque_svg_pixel_survives_both_pixel_formats() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_svg(dir.path(), "d.svg", RED_40X20);

        let rgba = decode(&path, (40, 20), PixelFormat::Rgba8).unwrap();
        assert_eq!(
            &rgba.rgba[..4],
            &[255, 0, 0, 255],
            "straight RGBA: opaque red stays opaque red"
        );

        let bgra = decode(&path, (40, 20), PixelFormat::BgraPremultiplied8).unwrap();
        assert_eq!(
            &bgra.rgba[..4],
            &[0, 0, 255, 255],
            "premultiplied BGRA: the channels swap and nothing is multiplied twice"
        );
    }

    /// A half-transparent pixel is the case that exposes a double
    /// premultiply: at alpha 128, red premultiplied once is ~128 and twice is
    /// ~64.
    #[test]
    fn a_semi_transparent_svg_pixel_is_premultiplied_exactly_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_svg(
            dir.path(),
            "t.svg",
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4">
                <rect x="0" y="0" width="4" height="4" fill="#ff0000" fill-opacity="0.5"/>
            </svg>"##,
        );

        let bgra = decode(&path, (4, 4), PixelFormat::BgraPremultiplied8).unwrap();
        let (b, g, r, a) = (bgra.rgba[0], bgra.rgba[1], bgra.rgba[2], bgra.rgba[3]);
        assert_eq!((b, g), (0, 0));
        assert!((120..=136).contains(&a), "half transparent, got alpha {a}");
        assert!(
            r.abs_diff(a) <= 2,
            "red premultiplied ONCE is ~alpha ({a}); got {r}.              Twice would be ~{}",
            (a as u16 * a as u16 / 255)
        );
    }

    /// Malformed SVG is a typed error and the alt text stands in — never a
    /// panic, exactly like a truncated PNG.
    #[test]
    fn a_malformed_svg_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_svg(dir.path(), "bad.svg", "<svg this is not xml");
        assert!(matches!(probe(&path), Err(MediaError::Undecodable(_))));
        assert!(decode(&path, (10, 10), PixelFormat::Rgba8).is_err());
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

        let first = cache
            .get(&p, (100, 100), PixelFormat::Rgba8)
            .await
            .expect("decodes");
        assert_eq!((first.width, first.height), (100, 50), "scaled to fit");
        assert_eq!(first.rgba.len(), 100 * 50 * 4, "RGBA8");

        let again = cache
            .get(&p, (100, 100), PixelFormat::Rgba8)
            .await
            .expect("decodes");
        assert!(Arc::ptr_eq(&first, &again), "second get is a cache hit");

        // A different target is a different decode, not a stale hit.
        let other = cache
            .get(&p, (40, 40), PixelFormat::Rgba8)
            .await
            .expect("decodes");
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
            cache
                .get(&p, (100, 100), PixelFormat::Rgba8)
                .await
                .expect("decodes");
            assert!(
                cache.bytes() <= 50_000,
                "over budget after {i}: {}",
                cache.bytes()
            );
        }
        cache.clear();
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn geometry_scales_to_the_pane_and_reserves_whole_rows() {
        use lattice_cells::MediaFit;
        // 400x200 in a 200px pane at 20px lines: halved to 200x100, which is
        // 5 line-heights, so 5 rows.
        let (rows, lh) = block_geometry((400, 200), MediaFit::Contain, 20.0, 200.0);
        assert_eq!(rows, 5);
        assert!((lh - 5.0).abs() < 0.001, "got {lh}");

        // Fractional heights are KEPT, not snapped — that is what makes the
        // block variable-height rather than whole-row. 4.25 draws 4.25 and
        // reserves 5, so it never paints over the line beneath it.
        let (rows, lh) = block_geometry((400, 170), MediaFit::Contain, 20.0, 200.0);
        assert!((lh - 4.25).abs() < 0.001, "got {lh}");
        assert_eq!(
            rows, 5,
            "rows is ceil(height), so the reservation covers it"
        );
    }

    /// `Contain` never scales up; `Width` does. A small icon blown up to fill
    /// the pane is worse than the icon.
    #[test]
    fn contain_never_upscales_but_width_does() {
        use lattice_cells::MediaFit;
        let (_, lh_contain) = block_geometry((32, 32), MediaFit::Contain, 20.0, 400.0);
        assert!(
            (lh_contain - 1.6).abs() < 0.001,
            "32px / 20px lines, got {lh_contain}"
        );

        let (_, lh_width) = block_geometry((32, 32), MediaFit::Width, 20.0, 400.0);
        assert!(
            (lh_width - 20.0).abs() < 0.001,
            "filled the width, got {lh_width}"
        );
    }

    /// Degenerate geometry must not divide by zero or reserve something
    /// absurd — an unmeasured pane resolves on the next frame.
    #[test]
    fn degenerate_geometry_yields_one_row_rather_than_nonsense() {
        use lattice_cells::MediaFit;
        for args in [
            ((100, 100), 0.0, 200.0),
            ((100, 100), 20.0, 0.0),
            ((0, 100), 20.0, 200.0),
            ((100, 0), 20.0, 200.0),
        ] {
            let (rows, lh) = block_geometry(args.0, MediaFit::Contain, args.1, args.2);
            assert_eq!((rows, lh), (1, 1.0), "for {args:?}");
        }
    }

    /// An extreme aspect ratio must not resolve to a sliver that is invisible
    /// while still occupying a row.
    #[test]
    fn an_extreme_ratio_still_draws_at_least_one_line() {
        use lattice_cells::MediaFit;
        let (rows, lh) = block_geometry((10_000, 1), MediaFit::Contain, 20.0, 200.0);
        assert!(lh >= MIN_BLOCK_LH, "got {lh}");
        assert_eq!(rows, 1);
    }

    /// GPUI needs premultiplied BGRA. Doing the swap at the consumer would
    /// put a per-pixel loop in the paint path — the exact thing this crate
    /// exists to keep out of it — so it happens inside the off-thread decode
    /// and is cached in that form.
    #[test]
    fn bgra_premultiplication_happens_at_decode_not_at_paint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("half.png");
        // One pixel: pure red at 50% alpha.
        image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 128]))
            .save(&path)
            .unwrap();

        let straight = decode(&path, (10, 10), PixelFormat::Rgba8).unwrap();
        assert_eq!(&straight.rgba[..], &[255, 0, 0, 128], "RGBA is untouched");

        let bgra = decode(&path, (10, 10), PixelFormat::BgraPremultiplied8).unwrap();
        // B, G, R, A with colour scaled by alpha: 255 * 128/255 = 128.
        assert_eq!(
            &bgra.rgba[..],
            &[0, 0, 128, 128],
            "channels swapped and premultiplied"
        );
    }

    /// Rounding rather than truncating: `(c * a) / 255` alone darkens every
    /// semi-transparent pixel by up to one level, which shows as a dingy
    /// edge on antialiased artwork.
    #[test]
    fn premultiplication_rounds_instead_of_truncating() {
        let mut px = vec![200u8, 100, 50, 200];
        premultiply_to_bgra(&mut px);
        // 200*200/255 = 156.86 -> 157 rounded (156 truncated).
        assert_eq!(px[2], 157, "red channel rounded");
        assert_eq!(px[3], 200, "alpha is left alone");
    }

    /// The format is part of the cache key: the same file wanted in two
    /// layouts is two decodes, not one served in the wrong byte order.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_format_is_part_of_the_cache_key() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_png(dir.path(), "c.png", 8, 8);
        let cache = Arc::new(MediaCache::new(8 * 1024 * 1024));

        let a = cache.get(&p, (100, 100), PixelFormat::Rgba8).await.unwrap();
        let b = cache
            .get(&p, (100, 100), PixelFormat::BgraPremultiplied8)
            .await
            .unwrap();
        assert!(
            !Arc::ptr_eq(&a, &b),
            "different layouts are different entries"
        );
        assert_eq!(a.format, PixelFormat::Rgba8);
        assert_eq!(b.format, PixelFormat::BgraPremultiplied8);
    }

    /// An edited image must reappear rather than serving the old decode
    /// forever — which is what `mtime` is in the key for.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rewriting_the_file_invalidates_its_entry() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_png(dir.path(), "x.png", 40, 40);
        let cache = Arc::new(MediaCache::new(8 * 1024 * 1024));
        let first = cache.get(&p, (100, 100), PixelFormat::Rgba8).await.unwrap();
        assert_eq!((first.width, first.height), (40, 40));

        // Rewrite at a different size, with an mtime the filesystem will
        // report as newer.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write_png(dir.path(), "x.png", 20, 20);

        let second = cache.get(&p, (100, 100), PixelFormat::Rgba8).await.unwrap();
        assert_eq!(
            (second.width, second.height),
            (20, 20),
            "the rewritten file was decoded again, not served from cache"
        );
    }
}
