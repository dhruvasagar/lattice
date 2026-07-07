//! Pure mapping from the `ui.window.decorations` option to GPUI window
//! chrome, per platform. Isolated + pure so it is testable without opening a
//! real window. See `docs/dev/architecture/gpui-window-chrome.md` for the
//! per-platform rationale (Linux X11 true borderless via Client CSD-off;
//! Windows borderless+resizable; macOS borderless via titlebar:None).

use gpui::{SharedString, TitlebarOptions};
use lattice_config::Decorations;

/// The default `full`-chrome titlebar used today by `run()`.
pub fn full_titlebar() -> TitlebarOptions {
    TitlebarOptions {
        title: Some(SharedString::from("Lattice")),
        ..Default::default()
    }
}

/// Map `decorations` to `(titlebar, window_decorations)` for `WindowOptions`.
///
/// - `full`  → `(Some(full_titlebar()), None)` — today's behavior.
/// - `none`  → `(None, Some(Client))` — `titlebar: None` drops OS chrome on
///   every platform; on Linux, `WindowDecorations::Client` additionally asks
///   the WM to strip server-side decorations (`_MOTIF_WM_HINTS` decorations=0
///   on X11 → true borderless). `window_decorations` is ignored on macOS /
///   Windows, so requesting `Client` there is harmless.
/// - `transparent` → a `Some(TitlebarOptions)` with `appears_transparent` but the
///   default traffic-light position. Because the titlebar is `Some`, GPUI keeps
///   the window resizable (`NSResizableWindowMask` on macOS), so edge-resize and
///   AX window managers (Raycast/yabai) work — unlike `none`. The buttons are
///   hidden separately, *after* the window opens, via [`hide_traffic_lights`]
///   (`setHidden:`) — NOT by moving them off-screen, which breaks AX so Raycast
///   can no longer set the window frame.
pub fn window_chrome(
    dec: Decorations,
) -> (Option<TitlebarOptions>, Option<gpui::WindowDecorations>) {
    match dec {
        Decorations::Full => (Some(full_titlebar()), None),
        Decorations::None_ => (None, Some(gpui::WindowDecorations::Client)),
        Decorations::Transparent => (
            Some(TitlebarOptions {
                title: Some(SharedString::from("Lattice")),
                appears_transparent: true,
                // Leave the buttons at their default position (do NOT move them):
                // `hide_traffic_lights` hides them with `setHidden:` after open,
                // which keeps their AX geometry sane so Raycast/yabai still work.
                ..Default::default()
            }),
            None,
        ),
    }
}

/// macOS: hide the three standard window buttons (close / miniaturize / zoom) on
/// the window backing `window` by sending `setHidden:` to each, reached through
/// gpui's raw window handle (an `NSView` pointer → its `NSWindow`). Used for
/// `ui.window.decorations = transparent` to get a buttonless, frameless-looking
/// window that stays resizable and controllable by AX window managers. No-op on
/// other platforms and a best-effort no-op if the handle can't be resolved.
#[cfg(target_os = "macos")]
pub fn hide_traffic_lights(window: &gpui::Window) {
    // objc message sends (see SAFETY below). Note: objc 0.2's `msg_send!` macro
    // emits a few benign `unexpected_cfg(cargo-clippy)` warnings from its own
    // expansion — third-party noise, not this code.
    #![allow(unsafe_code)]
    use objc::{msg_send, sel, sel_impl};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = HasWindowHandle::window_handle(window) else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    let ns_view: *mut objc::runtime::Object = appkit.ns_view.as_ptr().cast();
    // SAFETY: `ns_view` is a live `NSView` for the window's lifetime; we only send
    // `window` / `standardWindowButton:` / `setHidden:` to AppKit objects, on the
    // main thread (this runs inside a `window.update` on the GPUI foreground
    // executor). Null results are guarded before use.
    unsafe {
        let ns_window: *mut objc::runtime::Object = msg_send![ns_view, window];
        if ns_window.is_null() {
            return;
        }
        // NSWindowButton discriminants: Close = 0, Miniaturize = 1, Zoom = 2.
        for button_kind in 0u64..=2 {
            let button: *mut objc::runtime::Object =
                msg_send![ns_window, standardWindowButton: button_kind];
            if !button.is_null() {
                let _: () = msg_send![button, setHidden: true];
            }
        }
    }
}

/// Non-macOS: no traffic-light buttons to hide.
#[cfg(not(target_os = "macos"))]
pub fn hide_traffic_lights(_window: &gpui::Window) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_keeps_titlebar_and_no_decoration_override() {
        let (tb, dec) = window_chrome(Decorations::Full);
        assert!(tb.is_some());
        assert!(dec.is_none());
    }

    #[test]
    fn none_drops_titlebar_and_requests_client_csd() {
        let (tb, dec) = window_chrome(Decorations::None_);
        assert!(tb.is_none());
        assert_eq!(dec, Some(gpui::WindowDecorations::Client));
    }

    #[test]
    fn transparent_keeps_resizable_titlebar_and_default_button_position() {
        let (tb, dec) = window_chrome(Decorations::Transparent);
        // A `Some` titlebar keeps the window resizable (NSResizableWindowMask).
        let tb = tb.expect("transparent keeps a titlebar so the window stays resizable");
        assert!(tb.appears_transparent);
        // Buttons are NOT moved (that breaks AX/Raycast); they're hidden after
        // open via `hide_traffic_lights` (setHidden) instead.
        assert!(tb.traffic_light_position.is_none());
        assert!(dec.is_none());
    }
}
