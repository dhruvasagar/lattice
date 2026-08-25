//! IM.6b media fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `media-plugin` world,
//! driving the media producer actor (`media_task.rs`) through a real host→guest
//! `media-blocks` call:
//!
//!   - the producer returns blocks derived from the projected
//!     `decoration-context`: a RELATIVE path on line 1 (which the host must
//!     resolve against the buffer's directory) and an ABSOLUTE one on the last
//!     line (`line_count - 1`, proving `line_count` crossed in);
//!   - an empty buffer (`line_count == 0`) returns the WIT typed `err`,
//!     exercising the graceful "no blocks for this trigger" path, which the
//!     host maps to keeping the buffer's prior blocks rather than clearing them.

wit_bindgen::generate!({
    world: "media-plugin",
    path: "../../../../../wit",
});

use exports::lattice::plugin_host::media::Guest;
use lattice::plugin_host::types::{DecorationContext, MediaBlock, MediaFit};

struct Component;

impl Guest for Component {
    fn media_blocks(ctx: DecorationContext, _text: String) -> Result<Vec<MediaBlock>, String> {
        if ctx.line_count == 0 {
            // Graceful: nothing to scan → a typed guest err, not a trap.
            return Err("media-guest: empty buffer".to_string());
        }
        Ok(vec![
            MediaBlock {
                anchor_line: 1,
                // Relative — the host resolves it against the buffer's own
                // directory, which is what `[[file:diagram.png]]` means.
                path: "img/diagram.png".to_string(),
                alt: Some("a wiring diagram".to_string()),
                fit: MediaFit::Contain,
            },
            MediaBlock {
                anchor_line: ctx.line_count - 1,
                path: "/tmp/absolute.png".to_string(),
                // No alt — the host falls back to the file name.
                alt: None,
                fit: MediaFit::Width,
            },
        ])
    }
}

export!(Component);
