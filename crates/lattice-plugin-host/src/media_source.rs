//! IM.6b — the `WasmMediaSource` adapter.
//!
//! Wraps a media plugin's [`MediaClient`] bridge and exposes a **native**-typed
//! producer the host calls off the render path, exactly like
//! `WasmDecorationSource`. The renderer reads the cache; it never calls WASM.

use std::path::{Path, PathBuf};

use lattice_mode::media_source::{AsyncMediaSource, MediaBlockRequest, MediaFuture};

use crate::PluginId;
use crate::boundary_decoration::project_decoration_context;
use crate::media_task::MediaClient;

/// An async media-block producer over a plugin's [`MediaClient`].
#[derive(Clone, Debug)]
pub struct WasmMediaSource {
    client: MediaClient,
}

impl AsyncMediaSource for WasmMediaSource {
    fn source_id(&self) -> u64 {
        self.plugin_id().0 as u64
    }

    fn produce(
        &self,
        buffer_id: u64,
        path: Option<PathBuf>,
        line_count: u32,
        text: String,
    ) -> MediaFuture<'_> {
        Box::pin(async move {
            self.media_blocks(buffer_id, path.as_deref(), line_count, text)
                .await
        })
    }
}

impl WasmMediaSource {
    pub fn new(client: MediaClient) -> Self {
        Self { client }
    }

    pub fn plugin_id(&self) -> PluginId {
        self.client.id()
    }

    /// Produce a buffer's media blocks — the async producer the host calls OFF
    /// the render path.
    ///
    /// Graceful: the outer host error (trap / plugin-gone) and the inner guest
    /// WIT `err` both collapse to the `String` the caller logs, and on an `Err`
    /// the caller keeps the buffer's PRIOR blocks rather than clearing them. A
    /// transient failure mid-edit must not make every image in the document
    /// blink out.
    ///
    /// A block naming a path that does not resolve is DROPPED here with a
    /// warning rather than failing the batch — one broken link in an org file
    /// should not cost the reader every other image on the page.
    pub async fn media_blocks(
        &self,
        buffer_id: u64,
        path: Option<&Path>,
        line_count: u32,
        text: String,
    ) -> Result<Vec<MediaBlockRequest>, String> {
        let ctx = project_decoration_context(buffer_id, path, line_count);
        let wit: Vec<crate::media_task::MediaBlock> = match self.client.produce(ctx, text).await {
            Ok(inner) => inner?,
            Err(host_err) => return Err(format!("media plugin: {host_err}")),
        };
        // Relative paths resolve against the BUFFER's directory, which is what
        // an org `[[file:diagram.png]]` means — not the editor's cwd, which is
        // wherever the user happened to launch from.
        let base = path.and_then(|p| p.parent()).map(Path::to_path_buf);
        Ok(wit
            .into_iter()
            .filter_map(|b| resolve_block(b, base.as_deref()))
            .collect())
    }
}

/// Convert one WIT block, resolving its path. `None` drops the block.
fn resolve_block(
    b: crate::media_task::MediaBlock,
    base: Option<&Path>,
) -> Option<MediaBlockRequest> {
    if b.path.trim().is_empty() {
        tracing::warn!("media block with an empty path; dropped");
        return None;
    }
    let raw = PathBuf::from(&b.path);
    let resolved = if raw.is_absolute() {
        raw
    } else {
        match base {
            Some(dir) => dir.join(raw),
            // No buffer path to resolve against (a scratch buffer). A relative
            // reference is meaningless there, and guessing the cwd would open
            // whatever happens to sit beside the launch directory.
            None => {
                tracing::warn!(path = %b.path, "relative media path in a pathless buffer; dropped");
                return None;
            }
        }
    };
    Some(MediaBlockRequest {
        anchor_line: b.anchor_line,
        path: resolved,
        alt: b.alt.filter(|a| !a.trim().is_empty()),
        fit: match b.fit {
            crate::media_task::MediaFit::Contain => lattice_cells::MediaFit::Contain,
            crate::media_task::MediaFit::Width => lattice_cells::MediaFit::Width,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_task::{MediaBlock as WitBlock, MediaFit as WitFit};

    fn wit(path: &str) -> WitBlock {
        WitBlock {
            anchor_line: 3,
            path: path.to_string(),
            alt: None,
            fit: WitFit::Contain,
        }
    }

    /// `[[file:diagram.png]]` means "beside this org file", not "beside
    /// wherever the editor was launched from".
    #[test]
    fn a_relative_path_resolves_against_the_buffers_directory() {
        let base = Path::new("/home/u/notes");
        let got = resolve_block(wit("img/diagram.png"), Some(base)).expect("resolves");
        assert_eq!(got.path, Path::new("/home/u/notes/img/diagram.png"));
    }

    #[test]
    fn an_absolute_path_is_left_alone() {
        let got = resolve_block(wit("/tmp/x.png"), Some(Path::new("/home/u"))).unwrap();
        assert_eq!(got.path, Path::new("/tmp/x.png"));
    }

    /// Guessing the cwd would open whatever happens to sit beside the launch
    /// directory — a wrong image is worse than none.
    #[test]
    fn a_relative_path_in_a_pathless_buffer_is_dropped() {
        assert!(resolve_block(wit("diagram.png"), None).is_none());
    }

    #[test]
    fn an_empty_path_is_dropped_rather_than_resolved() {
        assert!(resolve_block(wit("   "), Some(Path::new("/a"))).is_none());
    }

    /// A blank alt is the same as none, so `MediaBlock::new`'s file-name
    /// fallback takes over rather than showing an empty box.
    #[test]
    fn a_blank_alt_becomes_none_so_the_filename_fallback_applies() {
        let mut b = wit("/x.png");
        b.alt = Some("  ".into());
        assert_eq!(resolve_block(b, None).unwrap().alt, None);
    }
}
