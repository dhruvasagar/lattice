//! PDF pages: measuring a document and rasterising one page, off the UI
//! thread (PF.1).
//!
//! Design: `docs/dev/architecture/pdf-mode.md` §3–§4.
//!
//! Two calls, the same split as [`probe`](crate::probe) and
//! [`decode`](crate::decode) for pictures:
//!
//! - [`PdfDocument::open`] parses the file and reads every page's size, in
//!   points. No page is drawn, so it is cheap (PF.0 measured 3.3 ms for 472
//!   pages), and it is what lets a viewer size every page before any page's
//!   pixels exist, so nothing reflows as pages land.
//! - [`PdfDocument::render_page`] rasterises one page at a target size.
//!
//! Both block, and both belong on `spawn_blocking`. Nothing here stops a
//! renderer calling them from a paint; the rule lives at the call site, as it
//! does for [`decode`](crate::decode).
//!
//! ## A page has no natural pixel size
//!
//! A picture's pixels are its natural size, and the crate never scales one
//! *up*: an icon blown across the pane is worse than the icon. A page is
//! different. It is vector, measured in points, and is rendered *at* whatever
//! size it is shown at. So [`page_fit`] scales up as readily as down, and the
//! never-upscale rule does not apply here. The caller decides the size, and
//! the size is what a viewer's zoom means.
//!
//! ## Threads
//!
//! `hayro_syntax::Pdf` is `Send + Sync`, so one parsed document can be shared
//! across the blocking threads that render its pages. hayro's `RenderCache`
//! holds `Rc`s and is neither, so each render builds its own and drops it; it
//! is never stored or shared.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::Arc;

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::vello_cpu::peniko::ImageAlphaType;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};

use crate::{DecodedImage, MediaError, PixelFormat, guard_size, swap_rb, unpremultiply};

/// Every page's size, in PDF points (1/72 inch), as it is displayed: after
/// the page's own `/Rotate` and crop box.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfInfo {
    /// One entry per page, in order: `(width, height)` in points.
    pub pages: Vec<(f32, f32)>,
}

/// A parsed PDF, ready to render pages from.
///
/// Holding one per open document, rather than re-reading the file for every
/// page, is what makes scrolling cheap: parsing is lazy, but it is not free.
pub struct PdfDocument {
    pdf: Pdf,
    info: PdfInfo,
}

impl std::fmt::Debug for PdfDocument {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PdfDocument")
            .field("pages", &self.info.pages.len())
            .finish()
    }
}

impl PdfDocument {
    /// Read and parse `path`, measuring every page.
    pub fn open(path: &Path) -> Result<Self, MediaError> {
        let data = std::fs::read(path).map_err(|e| MediaError::Unreadable(e.to_string()))?;
        Self::from_bytes(data)
    }

    /// Parse a PDF already in memory.
    ///
    /// A file with no pages is an error rather than an empty document: a
    /// viewer would have nothing to anchor its first line to, and the honest
    /// answer is that this is not a PDF anyone can read.
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, MediaError> {
        let pdf = catch_unwind(AssertUnwindSafe(|| Pdf::new(data)))
            .map_err(|_| MediaError::Undecodable("the PDF parser panicked".into()))?
            .map_err(|e| MediaError::Undecodable(format!("{e:?}")))?;
        let pages: Vec<(f32, f32)> = pdf
            .pages()
            .iter()
            .map(hayro::hayro_syntax::page::Page::render_dimensions)
            .collect();
        if pages.is_empty() {
            return Err(MediaError::Undecodable("the PDF has no pages".into()));
        }
        Ok(Self {
            pdf,
            info: PdfInfo { pages },
        })
    }

    /// Every page's size.
    pub fn info(&self) -> &PdfInfo {
        &self.info
    }

    /// Rasterise page `page` (0-based) to fit `target` (width, height in px),
    /// preserving its aspect ratio, on a white page.
    ///
    /// White because a page is paper: a PDF draws on an implied white sheet,
    /// and a transparent page shown over a dark theme would make black text
    /// unreadable. The result is therefore opaque.
    ///
    /// The returned size can differ from [`page_fit`]'s by a pixel, because the
    /// rasteriser rounds the scaled page down. It is the size actually drawn.
    pub fn render_page(
        &self,
        page: usize,
        target: (u32, u32),
        format: PixelFormat,
    ) -> Result<DecodedImage, MediaError> {
        let size_pt = *self.info.pages.get(page).ok_or_else(|| {
            MediaError::Undecodable(format!(
                "page {} of a {}-page PDF",
                page + 1,
                self.info.pages.len()
            ))
        })?;
        let (w, h) = page_fit(size_pt, target)?;
        let pages = self.pdf.pages();
        let page_ref = pages
            .get(page)
            .ok_or_else(|| MediaError::Undecodable(format!("page {} is missing", page + 1)))?;
        let pixmap_settings = PixmapSettings {
            x_scale: w as f32 / size_pt.0,
            y_scale: h as f32 / size_pt.1,
            bg_color: WHITE,
        };
        // A page hayro cannot draw is that page's alt text, never a dead
        // render thread: the other pages of the document are unaffected.
        let pixmap = catch_unwind(AssertUnwindSafe(|| {
            let cache = RenderCache::new();
            render(
                page_ref,
                &cache,
                &InterpreterSettings::default(),
                &RenderSettings::default(),
                &pixmap_settings,
            )
        }))
        .map_err(|_| MediaError::Undecodable(format!("page {} could not be drawn", page + 1)))?;

        let (w, h) = (u32::from(pixmap.width()), u32::from(pixmap.height()));
        // vello_cpu, like tiny-skia, hands back PREMULTIPLIED RGBA: the same
        // conversions as the SVG route, and for the same reason. Taken, not
        // copied: a 2× page is ~53 MB, and a copy of it is a measurable share
        // of the render.
        let mut bytes = pixmap.take_rgba8(ImageAlphaType::AlphaPremultiplied);
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
}

/// Parsed documents, kept so the pages of one PDF share a parse (PF.3b).
///
/// Keyed by path and modification time: a PDF rewritten on disk (export
/// recompiled it) has a new `mtime` and is parsed again, so a re-export
/// never draws pages from the old file. Bounded by count, not bytes, because
/// a parsed document is small next to the rasters it produces (hayro parses
/// lazily; the file's bytes are the bulk), and the handful of PDFs open at
/// once is what it has to cover.
pub struct DocumentCache {
    entries: std::sync::Mutex<Vec<CachedDocument>>,
    capacity: usize,
}

struct CachedDocument {
    path: std::path::PathBuf,
    mtime: Option<std::time::SystemTime>,
    doc: Arc<PdfDocument>,
}

/// The process's [`DocumentCache`]: four documents.
pub static DOCUMENTS: std::sync::LazyLock<DocumentCache> =
    std::sync::LazyLock::new(|| DocumentCache::new(4));

impl DocumentCache {
    /// An empty cache holding at most `capacity` documents.
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: std::sync::Mutex::new(Vec::new()),
            capacity: capacity.max(1),
        }
    }

    /// The parsed document at `path`, from the cache when its `mtime` still
    /// matches, otherwise read and parsed now. Blocking.
    ///
    /// The lock is not held across the parse, so two threads opening a new
    /// document may both parse it; the second insert wins and nothing is
    /// lost but the duplicated work. Holding it would make every render of
    /// every cached document wait behind one slow parse.
    pub fn get(&self, path: &Path) -> Result<Arc<PdfDocument>, MediaError> {
        let mtime = std::fs::metadata(path)
            .map_err(|e| MediaError::Unreadable(e.to_string()))?
            .modified()
            .ok();
        {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(i) = entries
                .iter()
                .position(|e| e.path == path && e.mtime == mtime)
            {
                // Most recently used goes last.
                let hit = entries.remove(i);
                let doc = Arc::clone(&hit.doc);
                entries.push(hit);
                return Ok(doc);
            }
        }
        let doc = Arc::new(PdfDocument::open(path)?);
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        // A stale entry for the same path is replaced, not kept beside it.
        entries.retain(|e| e.path != path);
        entries.push(CachedDocument {
            path: path.to_path_buf(),
            mtime,
            doc: Arc::clone(&doc),
        });
        while entries.len() > self.capacity {
            entries.remove(0);
        }
        Ok(doc)
    }

    /// How many documents are cached. For tests and diagnostics.
    pub fn len(&self) -> usize {
        self.entries.lock().map(|e| e.len()).unwrap_or(0)
    }

    /// True when nothing is cached.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The pixel size of a page of `size_pt` points fitted inside `target`,
/// preserving its aspect ratio, scaling up or down (see the module docs).
///
/// Refused above [`MAX_PIXELS`](crate::MAX_PIXELS), as a picture is, and above
/// 65 535 px on either side, the rasteriser's limit: a zoom that asks for more
/// shows the page's alt text rather than allocating without bound.
pub fn page_fit(size_pt: (f32, f32), target: (u32, u32)) -> Result<(u32, u32), MediaError> {
    let (pw, ph) = size_pt;
    let (tw, th) = target;
    if !(pw.is_finite() && ph.is_finite()) || pw <= 0.0 || ph <= 0.0 {
        return Err(MediaError::Undecodable("a page with no size".into()));
    }
    if tw == 0 || th == 0 {
        return Err(MediaError::Undecodable("a zero-sized target".into()));
    }
    let scale = (tw as f64 / pw as f64).min(th as f64 / ph as f64);
    // `max(1)`: a sliver of a page still gets a pixel, rather than vanishing.
    let w = ((pw as f64 * scale).round() as u32).max(1);
    let h = ((ph as f64 * scale).round() as u32).max(1);
    if w > u32::from(u16::MAX) || h > u32::from(u16::MAX) {
        return Err(MediaError::TooLarge {
            width: w,
            height: h,
        });
    }
    guard_size(w, h)?;
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal, valid PDF: one page per `(width, height, content)`, with an
    /// `ExtGState` named `/Half` (fill alpha 0.5) available to every page.
    /// The xref offsets are computed, so the file is well-formed rather than
    /// relying on a lenient parser to repair it.
    fn pdf_with_pages(pages: &[(f32, f32, &str)]) -> Vec<u8> {
        let n = pages.len();
        // Objects: 1 catalog, 2 pages, 3 gstate, then (page, content) pairs.
        let mut objects: Vec<String> = Vec::new();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
        objects.push(format!(
            "<< /Type /Pages /Kids [{}] /Count {n} >>",
            kids.join(" ")
        ));
        objects.push("<< /Type /ExtGState /ca 0.5 >>".into());
        for (i, (w, h, content)) in pages.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {w} {h}] \
                 /Resources << /ExtGState << /Half 3 0 R >> >> /Contents {} 0 R >>",
                5 + 2 * i
            ));
            objects.push(format!(
                "<< /Length {} >>\nstream\n{content}\nendstream",
                content.len() + 1
            ));
        }
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// A page filled with one colour, so any pixel tells the whole story.
    const RED_FILL: &str = "1 0 0 rg 0 0 1000 1000 re f";
    /// The same red at half alpha, drawn over the white page.
    const HALF_RED_FILL: &str = "/Half gs 1 0 0 rg 0 0 1000 1000 re f";

    fn pixel(img: &DecodedImage, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * img.width + x) * 4) as usize;
        [
            img.rgba[i],
            img.rgba[i + 1],
            img.rgba[i + 2],
            img.rgba[i + 3],
        ]
    }

    #[test]
    fn opening_measures_every_page_in_points_without_drawing() {
        let bytes = pdf_with_pages(&[(612.0, 792.0, RED_FILL), (842.0, 595.0, RED_FILL)]);
        let doc = PdfDocument::from_bytes(bytes).unwrap();
        assert_eq!(doc.info().pages, vec![(612.0, 792.0), (842.0, 595.0)]);
    }

    #[test]
    fn open_reads_from_disk_and_a_missing_file_is_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.pdf");
        std::fs::write(&path, pdf_with_pages(&[(100.0, 50.0, RED_FILL)])).unwrap();
        assert_eq!(PdfDocument::open(&path).unwrap().info().pages.len(), 1);
        assert!(matches!(
            PdfDocument::open(&dir.path().join("absent.pdf")),
            Err(MediaError::Unreadable(_))
        ));
    }

    /// Bytes that are not a PDF are the alt-text case, not a panic.
    #[test]
    fn garbage_is_undecodable_not_a_panic() {
        let err = PdfDocument::from_bytes(b"this is not a pdf".to_vec()).unwrap_err();
        assert!(matches!(err, MediaError::Undecodable(_)), "{err}");
    }

    #[test]
    fn a_page_past_the_end_is_an_error_naming_the_count() {
        let doc = PdfDocument::from_bytes(pdf_with_pages(&[(100.0, 50.0, RED_FILL)])).unwrap();
        let err = doc
            .render_page(1, (100, 100), PixelFormat::Rgba8)
            .unwrap_err();
        assert_eq!(
            err,
            MediaError::Undecodable("page 2 of a 1-page PDF".into())
        );
    }

    /// A page is rendered AT its target, so it scales up as well as down,
    /// unlike a picture, and keeps its aspect ratio either way.
    #[test]
    fn a_page_renders_at_the_fitted_size_up_or_down() {
        let doc = PdfDocument::from_bytes(pdf_with_pages(&[(100.0, 50.0, RED_FILL)])).unwrap();
        let small = doc.render_page(0, (50, 50), PixelFormat::Rgba8).unwrap();
        assert_eq!((small.width, small.height), (50, 25));
        let large = doc.render_page(0, (400, 1000), PixelFormat::Rgba8).unwrap();
        assert_eq!((large.width, large.height), (400, 200), "scaled UP");
        assert_eq!(large.rgba.len(), 400 * 200 * 4);
    }

    /// The colour survives in both layouts. An opaque fill pins the channel
    /// order; a half-alpha fill over the white page pins that the result is
    /// composited onto paper and opaque, with no alpha applied twice (which
    /// would darken it) or not at all (which would leave it transparent).
    #[test]
    fn colours_survive_in_both_pixel_formats() {
        let doc = PdfDocument::from_bytes(pdf_with_pages(&[
            (10.0, 10.0, RED_FILL),
            (10.0, 10.0, HALF_RED_FILL),
        ]))
        .unwrap();

        let rgba = doc.render_page(0, (10, 10), PixelFormat::Rgba8).unwrap();
        assert_eq!(pixel(&rgba, 5, 5), [255, 0, 0, 255]);
        let bgra = doc
            .render_page(0, (10, 10), PixelFormat::BgraPremultiplied8)
            .unwrap();
        assert_eq!(pixel(&bgra, 5, 5), [0, 0, 255, 255]);

        let half = doc.render_page(1, (10, 10), PixelFormat::Rgba8).unwrap();
        let [r, g, b, a] = pixel(&half, 5, 5);
        assert_eq!((r, a), (255, 255), "red over white, opaque");
        assert!(
            (126..=129).contains(&g) && g == b,
            "pink, not darkened: {g} {b}"
        );
    }

    #[test]
    fn page_fit_refuses_sizes_the_rasteriser_cannot_hold() {
        assert!(matches!(
            page_fit((612.0, 792.0), (100_000, 100_000)),
            Err(MediaError::TooLarge { .. })
        ));
        assert!(page_fit((0.0, 792.0), (100, 100)).is_err());
        assert!(page_fit((612.0, 792.0), (0, 100)).is_err());
        assert!(page_fit((f32::NAN, 1.0), (100, 100)).is_err());
    }

    #[test]
    fn a_sliver_of_a_page_still_gets_a_pixel() {
        assert_eq!(page_fit((10_000.0, 1.0), (100, 100)).unwrap(), (100, 1));
    }

    /// The cache shares one parse across a document's pages, re-parses a
    /// file rewritten on disk, and keeps no more documents than it may.
    #[test]
    fn the_document_cache_reuses_a_parse_until_the_file_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.pdf");
        std::fs::write(&path, pdf_with_pages(&[(100.0, 50.0, RED_FILL)])).unwrap();
        let cache = DocumentCache::new(2);

        let a = cache.get(&path).unwrap();
        let b = cache.get(&path).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "the second page reuses the parse");

        // A rewrite with a different mtime is a different document.
        std::fs::write(
            &path,
            pdf_with_pages(&[(100.0, 50.0, RED_FILL), (100.0, 50.0, RED_FILL)]),
        )
        .unwrap();
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(5);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(later)
            .unwrap();
        let c = cache.get(&path).unwrap();
        assert_eq!(
            c.info().pages.len(),
            2,
            "the rewritten file, not the stale parse"
        );
        assert_eq!(cache.len(), 1, "the stale entry was replaced");

        for name in ["q.pdf", "r.pdf"] {
            let p = dir.path().join(name);
            std::fs::write(&p, pdf_with_pages(&[(10.0, 10.0, RED_FILL)])).unwrap();
            cache.get(&p).unwrap();
        }
        assert_eq!(cache.len(), 2, "bounded by its capacity");
    }

    /// One parsed document serves renders on several threads at once: the
    /// property PF.3's per-buffer document relies on.
    #[test]
    fn one_document_renders_pages_from_several_threads() {
        let doc = Arc::new(
            PdfDocument::from_bytes(pdf_with_pages(&[
                (100.0, 100.0, RED_FILL),
                (100.0, 100.0, HALF_RED_FILL),
            ]))
            .unwrap(),
        );
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let doc = Arc::clone(&doc);
                std::thread::spawn(move || {
                    doc.render_page(i % 2, (50, 50), PixelFormat::Rgba8)
                        .map(|img| img.width)
                })
            })
            .collect();
        for h in handles {
            assert_eq!(h.join().unwrap().unwrap(), 50);
        }
    }
}
