//! PF.2: `pdf-mode`'s probe, over `lattice-media`.
//!
//! The mode lives in `lattice-mode`, which cannot depend on `lattice-media`
//! (the codec and PDF graph that crate keeps out of everyone's build); the
//! host can, and does already for images. So the host implements the mode's
//! [`PdfProbe`] seam and registers it at boot, in a build that reads PDFs.

use std::path::Path;

use lattice_mode::modes::PdfProbe;

/// [`PdfProbe`] backed by [`lattice_media::pdf_page_sizes`].
#[derive(Debug, Default)]
pub struct MediaPdfProbe;

impl PdfProbe for MediaPdfProbe {
    fn page_sizes(&self, path: &Path) -> Result<Vec<(f32, f32)>, String> {
        lattice_media::pdf_page_sizes(path).map_err(|e| e.to_string())
    }
}

/// The probe this build offers `pdf-mode`, or `None` when it reads no PDFs.
///
/// `None` is the TUI-only build, and `pdf-mode` shows one line saying so.
/// Registering a probe that always fails would show "not a readable PDF"
/// instead, which blames the file for the build.
pub fn pdf_probe() -> Option<lattice_mode::modes::PdfProbeHandle> {
    lattice_media::PDF_SUPPORTED
        .then(|| std::sync::Arc::new(MediaPdfProbe) as lattice_mode::modes::PdfProbeHandle)
}
