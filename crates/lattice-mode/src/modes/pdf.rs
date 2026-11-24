//! `pdf-mode` — the major mode for a buffer whose file is a PDF (PF.2).
//!
//! Design: `docs/dev/architecture/pdf-mode.md`.
//!
//! The same shape as [`image-mode`](super::image), with one difference that
//! decides most of this module: a picture is one block, and a PDF is one block
//! **per page**, each anchored to a line of its own. The buffer's text is one
//! label per page (`Page 3 of 12`), so the line number is the page number and
//! `42G`, `gg`, `G`, marks and the jump list are vim's own, unmodified.
//!
//! ## The labels arrive after the open
//!
//! The open path builds a presented buffer with one empty line and reads
//! nothing, because it runs on the editor actor. The page count needs the file,
//! so [`PdfMode`]'s activation hands the read to a blocking thread and writes
//! the labels when the count lands, through the document handle as any owner
//! of a buffer's text does. Line 0 exists from the first frame, so page 1's
//! block (PF.3) has its anchor before anything is read, and the lines that
//! arrive are all *below* it: nothing on screen moves.
//!
//! The write is announced with [`PdfPagesLabelled`], whose boot-registered wake
//! is what makes the labels appear without a keypress.
//!
//! ## The probe is a service
//!
//! Reading a PDF is `lattice-media`'s, and this crate does not depend on it:
//! `lattice-mode` is in nearly every crate's closure, and `lattice-media`
//! carries the image codecs and the PDF interpreter, which is exactly the
//! graph that crate exists to keep out of everyone else's build. So the mode
//! asks for a [`PdfProbeHandle`], which the host registers when its build
//! reads PDFs. No handle is the TUI-only build, and the buffer says so in one
//! line rather than pretending the file is empty.
//!
//! ## It presents, it does not edit
//!
//! Read-only three times over, for `image-mode`'s reason: the text is a page
//! list, and saving it would replace the PDF with that list.
//! [`ReadOnly`](lattice_config::ReadOnly) gates typing, the implied
//! `read-only-mode` refuses the operators and undo, and `do_write` refuses the
//! save.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{
    BufferStoreHandle, CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    OptionOverrideSet,
};

/// Extensions `pdf-mode` claims.
pub const PDF_EXTENSIONS: &[&str] = &["pdf"];

/// Reads a PDF's page sizes for `pdf-mode`.
///
/// Implemented by the host over `lattice-media` (see the module docs for why
/// this crate cannot call it directly) and registered only in a build that
/// reads PDFs.
pub trait PdfProbe: Send + Sync {
    /// Every page's `(width, height)` in points, in order. **Blocking**: a
    /// file read and a parse. Never called on the editor actor.
    ///
    /// An `Err` is shown to the user in place of the pages, so it should say
    /// what is wrong with the file.
    fn page_sizes(&self, path: &Path) -> Result<Vec<(f32, f32)>, String>;
}

/// Boot-service handle for [`PdfProbe`]. Register **and** look up with this
/// exact alias (the `ServiceRegistry` TypeId rule).
pub type PdfProbeHandle = Arc<dyn PdfProbe>;

/// Published when a `pdf-mode` buffer's text has been written: its labels,
/// or the one line that stands in for them.
///
/// Its subscriber is the boot-registered wake, which is what puts the labels
/// on screen without a keypress. PF.3's page blocks follow from the same
/// edit: the media pump runs on the document version it moved.
#[derive(Debug, Clone)]
pub struct PdfPagesLabelled {
    /// The buffer whose text was written.
    pub buffer: lattice_core::BufferId,
    /// How many pages it has; 0 when the file could not be read.
    pub pages: usize,
}

lattice_protocol::register_event!(
    PdfPagesLabelled,
    "pdf.pages-labelled",
    "A pdf-mode buffer's page labels were written.",
    "lattice-mode",
);

/// Major mode for a buffer backed by a PDF.
pub struct PdfMode;

impl PdfMode {
    /// The canonical id, `"pdf-mode"`.
    pub fn mode_id() -> ModeId {
        ModeId::new("pdf-mode")
    }
}

impl Mode for PdfMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Major
    }

    fn required_capabilities(&self) -> CapabilitySet {
        CapabilitySet::empty()
    }

    fn presents_extensions(&self) -> &[&'static str] {
        PDF_EXTENSIONS
    }

    /// Read-only, half one: this gates insert-mode typing.
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
        }
    }

    /// Read-only, half two: `read-only-mode` carries the invocation runner
    /// that refuses the operators. On the major, because an implied mode is
    /// followed from the mode being activated.
    fn implies(&self) -> &[ModeId] {
        static IMPLIED: std::sync::OnceLock<Vec<ModeId>> = std::sync::OnceLock::new();
        IMPLIED.get_or_init(|| vec![crate::modes::ReadOnlyMode::mode_id()])
    }

    /// Start the probe and return at once: the labels land later, off the
    /// actor, and announce themselves (module docs).
    fn on_activate(&self, ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        let buffer = lattice_core::BufferId(ctx.buffer_id().0 as u32);
        let store = ctx.service::<BufferStoreHandle>();
        // The registry hands back `Arc<PdfProbeHandle>`; one layer off.
        let probe = ctx.service::<PdfProbeHandle>().map(|h| Arc::clone(&*h));
        let events = ctx.events_handle();
        Box::pin(async move {
            let Some(store) = store else {
                // A harness with no buffer store; nothing to write into.
                tracing::debug!("pdf-mode: no buffer store; labels skipped");
                return Ok(());
            };
            let (Some(path), Some(handle)) = (store.path_for(buffer), store.handle_for(buffer))
            else {
                tracing::debug!(?buffer, "pdf-mode: buffer has no path or vanished");
                return Ok(());
            };
            lattice_runtime::runtime::spawn_on_lsp_runtime(async move {
                let (text, pages) = read_labels(probe, path).await;
                write_owned_text(&*handle, text).await;
                events.publish_typed(PdfPagesLabelled { buffer, pages });
            });
            Ok(())
        })
    }
}

/// The buffer's text for the PDF at `path`, and its page count.
///
/// Never empty: a file that cannot be read gets one line naming it and the
/// reason, the PDF counterpart of an image's alt text. An empty buffer would
/// say nothing, and look like a PDF with no pages.
async fn read_labels(probe: Option<PdfProbeHandle>, path: PathBuf) -> (String, usize) {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let Some(probe) = probe else {
        return (
            format!("{name}: this build of lattice does not read PDFs; the GUI build does"),
            0,
        );
    };
    match lattice_runtime::runtime::spawn_blocking_on_lsp_runtime(move || probe.page_sizes(&path))
        .await
    {
        Ok(Ok(sizes)) if !sizes.is_empty() => (page_labels(sizes.len()), sizes.len()),
        Ok(Ok(_)) => (format!("{name}: a PDF with no pages"), 0),
        Ok(Err(reason)) => (format!("{name}: not a readable PDF ({reason})"), 0),
        Err(join) => {
            tracing::warn!(error = %join, "pdf-mode: the probe thread failed");
            (format!("{name}: reading the PDF failed"), 0)
        }
    }
}

/// One label per page, `Page N of M`, newline-joined: line `N - 1` is page `N`.
pub fn page_labels(pages: usize) -> String {
    (1..=pages)
        .map(|n| format!("Page {n} of {pages}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Replace the buffer's whole text with `text`, then declare it clean.
///
/// Clean, because the modified flag means "the user changed this" and the
/// user changed nothing: without it every PDF would open marked modified and
/// `:q` would refuse to leave it.
async fn write_owned_text(handle: &dyn lattice_runtime::Document, text: String) {
    let snap = handle.snapshot();
    let last_line = snap.buffer.rope_line_count().saturating_sub(1);
    let last_len = snap.buffer.line(last_line).unwrap_or_default().len() as u32;
    let all = lattice_protocol::Range::new(
        lattice_protocol::position::Position::new(0, 0),
        lattice_protocol::position::Position::new(last_line, last_len),
    );
    if let Err(e) = handle
        .apply_edit_batch(vec![lattice_protocol::edit::Edit::replace(all, text)])
        .await
    {
        tracing::warn!(error = %e, "pdf-mode: writing the page labels failed");
        return;
    }
    if let Err(e) = handle.mark_clean().await {
        tracing::debug!(error = %e, "pdf-mode: marking the labels clean failed");
    }
}

/// The media producer that draws a `pdf-mode` buffer's pages (PF.3).
///
/// One block per page, each anchored to its label line, through the same
/// registry, sizing and off-thread decode as `image-mode`'s picture. Sizes
/// come from the host's measure of the page in points; pixels from the
/// drawing peer, which renders only the pages it shows.
#[derive(Debug)]
pub struct PdfPagesMediaSource;

/// This producer's teardown key, far from any sequential `PluginId` (see
/// [`IMAGE_FILE_SOURCE_ID`](super::image::IMAGE_FILE_SOURCE_ID)).
pub const PDF_PAGES_SOURCE_ID: u64 = 0x5044_4650_4147_0001; // "PDFPAG"

impl crate::media_source::AsyncMediaSource for PdfPagesMediaSource {
    fn source_id(&self) -> u64 {
        PDF_PAGES_SOURCE_ID
    }

    fn produce(
        &self,
        _buffer_id: u64,
        path: Option<PathBuf>,
        line_count: u32,
        text: String,
    ) -> crate::media_source::MediaFuture<'_> {
        let blocks = path
            .filter(|p| is_pdf_path(p))
            .map(|p| page_blocks(&p, line_count, &text))
            .unwrap_or_default();
        Box::pin(async move { Ok(blocks) })
    }
}

/// One block per page, once the labels are written.
///
/// Nothing before then, and nothing for the one line that stands in for an
/// unreadable file: a block per line of THAT would be a page that does not
/// exist. The labels are recognised by their first line, which names the
/// count, so a buffer whose text is not a page list for exactly its own line
/// count draws nothing. Every ordinary buffer pays one extension comparison.
fn page_blocks(
    path: &Path,
    line_count: u32,
    text: &str,
) -> Vec<crate::media_source::MediaBlockRequest> {
    let first = text.lines().next().unwrap_or_default();
    if line_count == 0 || first != format!("Page 1 of {line_count}") {
        return Vec::new();
    }
    (0..line_count)
        .map(|page| crate::media_source::MediaBlockRequest {
            anchor_line: page,
            source: lattice_cells::MediaSource::PdfPage {
                path: path.to_path_buf(),
                page,
            },
            // `None`: `page N of name.pdf` is the fallback, and is what a
            // page that cannot be drawn should say.
            alt: None,
            // Fit page, design §2's default. PF.4 makes it the buffer's
            // `pdf.zoom`.
            fit: lattice_cells::MediaFit::Page,
        })
        .collect()
}

/// Register [`PdfPagesMediaSource`] against the boot-time media registry.
/// Here, with the mode, for `image-mode`'s reason: the major and the producer
/// that draws its buffers are one surface.
pub fn register_pdf_media_source(registry: &crate::media_source::MediaSourceRegistryHandle) {
    let producer: Arc<dyn crate::media_source::AsyncMediaSource> = Arc::new(PdfPagesMediaSource);
    registry.rcu(|current| {
        let mut next = (**current).clone();
        next.register(producer.clone());
        Arc::new(next)
    });
}

/// True when `path` is a file `pdf-mode` presents.
pub fn is_pdf_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| PDF_EXTENSIONS.iter().any(|p| e.eq_ignore_ascii_case(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_kind_and_claim() {
        assert_eq!(PdfMode.id().as_str(), "pdf-mode");
        assert_eq!(PdfMode.kind(), ModeKind::Major);
        assert_eq!(PdfMode.presents_extensions(), &["pdf"]);
        assert!(is_pdf_path(Path::new("/a/Paper.PDF")), "case-insensitive");
        assert!(!is_pdf_path(Path::new("/a/notes.org")));
        assert!(!is_pdf_path(Path::new("/a/pdf")), "no extension at all");
    }

    /// The text is a page list, so a write would replace the PDF with it.
    /// Both declarations are needed: the option gates typing only.
    #[test]
    fn read_only_is_declared_twice() {
        assert!(
            PdfMode
                .options()
                .iter()
                .any(|o| o.key == std::any::TypeId::of::<lattice_config::ReadOnly>())
        );
        assert!(
            PdfMode
                .implies()
                .contains(&crate::modes::ReadOnlyMode::mode_id())
        );
    }

    #[test]
    fn line_n_is_page_n_plus_one() {
        assert_eq!(page_labels(1), "Page 1 of 1");
        let three = page_labels(3);
        assert_eq!(
            three.lines().collect::<Vec<_>>(),
            ["Page 1 of 3", "Page 2 of 3", "Page 3 of 3"]
        );
    }

    struct Fixed(Result<Vec<(f32, f32)>, String>);
    impl PdfProbe for Fixed {
        fn page_sizes(&self, _: &Path) -> Result<Vec<(f32, f32)>, String> {
            self.0.clone()
        }
    }

    fn probe(r: Result<Vec<(f32, f32)>, String>) -> Option<PdfProbeHandle> {
        Some(Arc::new(Fixed(r)))
    }

    /// A block per labelled line, each naming its own page, and nothing for a
    /// buffer that is not (yet) a page list: before the labels land, the
    /// one-line error, or any other file.
    #[tokio::test]
    async fn the_producer_draws_one_page_per_label_and_nothing_else() {
        use crate::media_source::AsyncMediaSource;
        let src = PdfPagesMediaSource;
        let path: PathBuf = "/a/paper.pdf".into();

        let blocks = src
            .produce(1, Some(path.clone()), 3, page_labels(3))
            .await
            .unwrap();
        assert_eq!(blocks.len(), 3);
        for (i, b) in blocks.iter().enumerate() {
            assert_eq!(b.anchor_line, i as u32, "page N hangs below line N");
            assert_eq!(
                b.source,
                lattice_cells::MediaSource::PdfPage {
                    path: path.clone(),
                    page: i as u32
                }
            );
            assert_eq!(b.fit, lattice_cells::MediaFit::Page);
        }

        for (p, lines, text) in [
            (Some(path.clone()), 1, String::new()),
            (
                Some(path.clone()),
                1,
                "paper.pdf: not a readable PDF (bad xref)".to_string(),
            ),
            (Some(path.clone()), 2, page_labels(3)),
            (Some("/a/notes.org".into()), 3, page_labels(3)),
            (None, 3, page_labels(3)),
        ] {
            let blocks = src.produce(1, p, lines, text).await.unwrap();
            assert!(blocks.is_empty(), "{blocks:?}");
        }
    }

    #[tokio::test]
    async fn a_readable_pdf_gets_one_label_per_page() {
        let (text, pages) =
            read_labels(probe(Ok(vec![(612.0, 792.0); 4])), "/a/paper.pdf".into()).await;
        assert_eq!(pages, 4);
        assert_eq!(text.lines().count(), 4);
        assert_eq!(text.lines().last(), Some("Page 4 of 4"));
    }

    /// A broken file and a build without PDF support each say so, in one
    /// line naming the file: never an empty buffer.
    #[tokio::test]
    async fn a_file_that_cannot_be_read_says_why_in_one_line() {
        let (broken, pages) =
            read_labels(probe(Err("bad xref".into())), "/a/paper.pdf".into()).await;
        assert_eq!(pages, 0);
        assert_eq!(broken, "paper.pdf: not a readable PDF (bad xref)");

        let (unsupported, _) = read_labels(None, "/a/paper.pdf".into()).await;
        assert_eq!(
            unsupported,
            "paper.pdf: this build of lattice does not read PDFs; the GUI build does"
        );

        let (empty, _) = read_labels(probe(Ok(vec![])), "/a/paper.pdf".into()).await;
        assert_eq!(empty, "paper.pdf: a PDF with no pages");
    }
}
