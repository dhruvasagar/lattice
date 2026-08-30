//! MV.1 multibuffer-view fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the
//! `multibuffer-view-plugin` world, to drive the per-plugin actor bridge
//! (`MultibufferViewActor` + `MultibufferViewClient`) through a real guest:
//!
//!   - `register-multibuffer-views()` declares **two** views, because one is
//!     the case a one-view-per-component seam would already have handled and
//!     the second is the whole reason the seam is registry-shaped.
//!   - `build(view, args)` echoes its inputs into the excerpts it returns, so
//!     the host can assert both crossed: the view name lands in the first
//!     excerpt's header and the joined `args` in the second's.
//!   - `args` containing `"fail"` returns the WIT typed `err`, which proves the
//!     guest-decline path is distinct from a host trap — the difference between
//!     "this view has nothing to show you and here is why" and "this plugin is
//!     broken".

wit_bindgen::generate!({
    world: "multibuffer-view-plugin",
    path: "../../../../../wit",
});

use exports::lattice::plugin_host::multibuffer_view_source::Guest as ViewGuest;
use lattice::plugin_host::multibuffer_view_registry::register_multibuffer_view;
use lattice::plugin_host::types::{
    MultibufferViewExcerpt, MultibufferViewInput, MultibufferViewResult, MultibufferViewSpec,
};

const PULL_VIEW: &str = "fixture-pull";
const SCAN_VIEW: &str = "fixture-scan";

struct Component;

impl Guest for Component {
    fn register_multibuffer_views() {
        register_multibuffer_view(&MultibufferViewSpec {
            id: PULL_VIEW.to_string(),
            doc: "Fixture pull view (MV.1 substrate validation)".to_string(),
            buffer_name: "*fixture-pull*".to_string(),
            view_mode: Some("fixture-view-mode".to_string()),
            reuse: true,
            input: MultibufferViewInput::Pull,
        });
        // A second view, declared by the SAME component — the property the
        // registry shape exists for.
        register_multibuffer_view(&MultibufferViewSpec {
            id: SCAN_VIEW.to_string(),
            doc: "Fixture scan view".to_string(),
            buffer_name: "*fixture-scan*".to_string(),
            view_mode: None,
            reuse: false,
            input: MultibufferViewInput::Scan,
        });
        // An unnamed view: the host must refuse this one and keep the two
        // above, rather than dropping the plugin's whole contribution.
        register_multibuffer_view(&MultibufferViewSpec {
            id: String::new(),
            doc: String::new(),
            buffer_name: String::new(),
            view_mode: None,
            reuse: true,
            input: MultibufferViewInput::Pull,
        });
    }
}

impl ViewGuest for Component {
    fn build(view: String, args: Vec<String>) -> Result<MultibufferViewResult, String> {
        if args.iter().any(|a| a == "fail") {
            return Err(format!("fixture view `{view}` declined"));
        }
        Ok(MultibufferViewResult {
            excerpts: vec![
                MultibufferViewExcerpt {
                    path: "a.txt".to_string(),
                    start_line: 0,
                    end_line: 1,
                    // Echoes the view name, so the host can assert WHICH view
                    // was asked for crossed the boundary.
                    header: format!("view:{view}"),
                    match_count: Some(2),
                },
                MultibufferViewExcerpt {
                    path: "b.txt".to_string(),
                    start_line: 2,
                    end_line: 2,
                    // Echoes the args. Empty header on a real grouped view
                    // means "same group as the row above"; here it is just the
                    // second row's payload.
                    header: format!("args:{}", args.join(",")),
                    match_count: None,
                },
            ],
            summary: format!("{} excerpts", 2),
        })
    }
}

export!(Component);
