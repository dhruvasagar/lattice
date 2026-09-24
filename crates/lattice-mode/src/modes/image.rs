//! `image-mode` — the major mode for a buffer whose file is a picture.
//!
//! `:e diagram.png` used to fail on the UTF-8 read: `Document::open` is
//! `read_to_string`, and a PNG is not text. This mode is the other answer —
//! the file becomes an ordinary buffer with an ordinary major, listed by
//! `:ls`, reached by `:bn`, named in the modeline — and what it shows is the
//! image, through the same inline-media substrate an org `[[file:…]]` block
//! uses.
//!
//! ## It presents, it does not edit
//!
//! [`Mode::presents_extensions`] is what tells the open path not to read the
//! bytes. The buffer holds a single empty line and the picture hangs below it
//! as a media block, so nothing about the file's contents is in the rope.
//!
//! That makes read-only **load-bearing rather than tidy**: the buffer's text
//! is not the file, so writing it back would replace the image with nothing.
//! Both declarations are present and both are needed —
//! [`ReadOnly`](lattice_config::ReadOnly) gates insert-mode typing, and the
//! implied `read-only-mode` carries the invocation runner that refuses `dd`,
//! `x`, `cw` and `p`. The option alone would let an operator through.
//!
//! The third gate is not here: `:w` is refused host-side, because a save does
//! not go through either of the above.

use crate::{
    CapabilitySet, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind, OptionOverrideSet,
};

/// Extensions `image-mode` claims.
///
/// The same set org's inline-image scanner allows, and deliberately so: a
/// file that draws inline must draw when opened directly, or the two surfaces
/// disagree about what an image is. An allow-list rather than sniffing — a
/// file is opened because the user asked for it, and guessing at its type by
/// reading it is how a text file with a stray byte becomes a broken picture.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "svg", "bmp"];

/// Major mode for a buffer backed by an image file.
pub struct ImageMode;

impl ImageMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("image-mode")
    }
}

impl Mode for ImageMode {
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
        IMAGE_EXTENSIONS
    }

    /// Read-only, half one: this gates insert-mode typing.
    fn options(&self) -> OptionOverrideSet {
        lattice_config::overrides! {
            lattice_config::ReadOnly = true,
        }
    }

    /// Read-only, half two: `read-only-mode` carries the invocation runner
    /// that refuses the operators. Declared on the MAJOR, because an implied
    /// mode is followed from the mode being activated — putting it on a
    /// shared minor looks tidier and does not fire.
    fn implies(&self) -> &[ModeId] {
        static IMPLIED: std::sync::OnceLock<Vec<ModeId>> = std::sync::OnceLock::new();
        IMPLIED.get_or_init(|| vec![crate::modes::ReadOnlyMode::mode_id()])
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        // Nothing to set up: the picture comes from the media source this
        // module registers at boot, which reads the buffer's path.
        Box::pin(async { Ok(()) })
    }
}

/// The media producer that draws an `image-mode` buffer's own file.
///
/// The mode owns its picture the way org owns its inline ones: through the
/// media-source registry, off the render path, sized and decoded by the host.
/// There is no image-specific code in the renderer or the host because of it —
/// an `image-mode` buffer is one block anchored at line 0, which is the same
/// thing an org buffer produces several of.
#[derive(Debug)]
pub struct ImageFileMediaSource;

/// This producer's teardown key.
///
/// Deliberately far from any `PluginId`, which are small sequential integers:
/// the registry is keyed by `source_id` and a collision would make a plugin
/// reload silently unregister this.
pub const IMAGE_FILE_SOURCE_ID: u64 = 0x494D_4147_4500_0001; // "IMAGE"

impl crate::media_source::AsyncMediaSource for ImageFileMediaSource {
    fn source_id(&self) -> u64 {
        IMAGE_FILE_SOURCE_ID
    }

    fn produce(
        &self,
        _buffer_id: u64,
        path: Option<std::path::PathBuf>,
        _line_count: u32,
        _text: String,
    ) -> crate::media_source::MediaFuture<'_> {
        // `Ok(vec![])` rather than `Err` for a buffer that is not an image:
        // an error means "keep what you had", and the truthful answer here is
        // "I looked, there is nothing of mine". Every ordinary buffer in the
        // editor takes this path, so it must also be free — it is one
        // extension comparison and no I/O.
        let block =
            path.filter(|p| is_image_path(p))
                .map(|p| crate::media_source::MediaBlockRequest {
                    anchor_line: 0,
                    path: p,
                    // `None`, so `MediaBlock::new` falls back to the file name.
                    // That name is what shows if the picture cannot be decoded,
                    // and for a buffer whose whole content IS the file, the file
                    // name is the most useful thing to say.
                    alt: None,
                    // Never upscale: an icon blown up to fill the pane is worse
                    // than the icon.
                    fit: lattice_cells::MediaFit::Contain,
                });
        Box::pin(async move { Ok(block.into_iter().collect()) })
    }
}

/// Register [`ImageFileMediaSource`] against the boot-time media registry.
///
/// Called from the host's boot beside the registry's creation. It lives here,
/// with the mode, rather than in the host: the major and the producer that
/// draws its buffers are one surface, and splitting them is how half of a
/// mode ends up in the host.
pub fn register_image_media_source(registry: &crate::media_source::MediaSourceRegistryHandle) {
    let producer: std::sync::Arc<dyn crate::media_source::AsyncMediaSource> =
        std::sync::Arc::new(ImageFileMediaSource);
    registry.rcu(|current| {
        let mut next = (**current).clone();
        next.register(producer.clone());
        std::sync::Arc::new(next)
    });
}

/// True when `path` is a file `image-mode` presents.
///
/// Shared by the mode's claim and its media producer so the two cannot
/// disagree about which files are pictures.
pub fn is_image_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_and_kind() {
        assert_eq!(ImageMode.id().as_str(), "image-mode");
        assert_eq!(ImageMode.kind(), ModeKind::Major);
    }

    /// The buffer's text is a placeholder, so a write would replace the image
    /// with nothing. BOTH declarations are required: the option gates typing
    /// and nothing else, and operators reach the document through their own
    /// path.
    #[test]
    fn read_only_is_declared_twice_because_one_declaration_is_not_enough() {
        let opts = ImageMode.options();
        assert!(
            opts.iter()
                .any(|o| o.option_type_id == std::any::TypeId::of::<lattice_config::ReadOnly>()),
            "the ReadOnly option gates insert-mode typing"
        );
        assert!(
            ImageMode
                .implies()
                .contains(&crate::modes::ReadOnlyMode::mode_id()),
            "read-only-mode carries the invocation runner that refuses operators"
        );
    }

    /// The producer answers for its own buffer and stays silent — not
    /// errorful — everywhere else. An `Err` means "keep the blocks you had",
    /// which for an ordinary text buffer would be a lie.
    #[tokio::test]
    async fn the_producer_emits_one_block_for_an_image_buffer_and_none_otherwise() {
        use crate::media_source::AsyncMediaSource;
        let src = ImageFileMediaSource;

        let blocks = src
            .produce(1, Some("/a/shot.png".into()), 1, String::new())
            .await
            .expect("an image buffer is not an error");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].anchor_line, 0, "the block hangs below line 0");
        assert_eq!(blocks[0].path, std::path::Path::new("/a/shot.png"));
        assert_eq!(blocks[0].alt, None, "so the file-name fallback applies");

        for other in [Some("/a/notes.org".into()), None] {
            let blocks: Vec<_> = src
                .produce(1, other, 1, String::new())
                .await
                .expect("a non-image buffer is not an error either");
            assert!(blocks.is_empty());
        }
    }

    #[test]
    fn it_claims_the_extensions_org_draws_inline() {
        assert!(is_image_path(std::path::Path::new("/a/b.png")));
        assert!(
            is_image_path(std::path::Path::new("/a/B.JPEG")),
            "case-insensitive"
        );
        assert!(!is_image_path(std::path::Path::new("/a/notes.org")));
        assert!(
            !is_image_path(std::path::Path::new("/a/png")),
            "no extension at all"
        );
    }
}
