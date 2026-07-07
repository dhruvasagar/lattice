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
/// - `transparent` → a `Some(TitlebarOptions)` with `appears_transparent` and
///   the traffic-light buttons parked far off-screen. Because the titlebar is
///   `Some`, GPUI keeps the window resizable (`NSResizableWindowMask` on macOS),
///   so edge-resize and AX window managers (Raycast/yabai) work — unlike `none`.
///   The transparent titlebar + hidden buttons make it look frameless.
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
                // Park the standard window buttons far off-screen so the window
                // looks buttonless while staying resizable. GPUI re-applies this
                // position on resize/activate, so it persists.
                traffic_light_position: Some(gpui::point(gpui::px(-1000.0), gpui::px(-1000.0))),
            }),
            None,
        ),
    }
}

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
    fn transparent_keeps_resizable_titlebar_with_hidden_lights() {
        let (tb, dec) = window_chrome(Decorations::Transparent);
        // A `Some` titlebar keeps the window resizable (NSResizableWindowMask).
        let tb = tb.expect("transparent keeps a titlebar so the window stays resizable");
        assert!(tb.appears_transparent);
        // Buttons parked off-screen (negative coords) → looks buttonless.
        assert!(tb.traffic_light_position.is_some());
        assert!(dec.is_none());
    }
}
