//! CR.4 dashboard fixture guest.
//!
//! Declares three sections, chosen to cover what can fail silently:
//!
//!   - `recent` — an ordinary added section. Renders text that DEPENDS ON
//!     `ctx`: the icon switches on `nerd-fonts` and the version is echoed.
//!     That is the whole reason this seam keeps a live guest rather than
//!     taking a fragment once at load, so the test asserts the output
//!     actually changes when the ctx does.
//!   - `getting-started` — deliberately reuses a BUILT-IN section id. Section
//!     ids are not namespaced because replacing a builtin is a supported
//!     capability, so this must shadow the builtin while loaded and give it
//!     back on unload.
//!   - `""` (empty id) — rejected host-side, and must not cost the other two.
//!
//! `render-section` also answers an unknown id with an empty fragment rather
//! than trapping, which the host relies on.

wit_bindgen::generate!({
    world: "dashboard-plugin",
    path: "../../../../../wit",
});

// `Ctx` and `Fragment` are NOT imported here: the world declares
// `use dashboard.{ctx, fragment}`, so bindgen already brings both into this
// module's scope. Importing them again is an E0255 name collision, not a
// convenience.
use lattice::plugin_host::dashboard::{register_section, Align, LinkTarget, Role, Row, Span};

struct Component;

fn line(text: String, role: Role) -> Row {
    Row {
        spans: vec![Span {
            text,
            role,
            link: None,
        }],
        align: Align::Left,
    }
}

impl Guest for Component {
    fn register_dashboard_sections() {
        let _ = register_section("recent", 15, true);
        // Not namespaced — this is meant to displace the builtin.
        let _ = register_section("getting-started", 20, true);
        // Rejected host-side; must not cost the two above.
        let _ = register_section("", 30, true);
    }

    fn render_section(id: String, ctx: Ctx) -> Fragment {
        match id.as_str() {
            "recent" => {
                // The ctx-dependent bit. Both glyphs are one cell wide, per
                // the icon-degradation rule — a section whose fallback is a
                // different width shifts the page's geometry on toggle.
                let icon = if ctx.nerd_fonts { "\u{f07b}" } else { "\u{25c6}" };
                Fragment {
                    rows: vec![
                        line("Recent".to_string(), Role::SectionHeading),
                        line(format!("{icon} project-one"), Role::Body),
                        line(format!("lattice {}", ctx.version), Role::Hint),
                        Row {
                            spans: vec![Span {
                                text: ":tutor".to_string(),
                                role: Role::Link,
                                link: Some(LinkTarget::Command("tutor".to_string())),
                            }],
                            align: Align::Left,
                        },
                    ],
                }
            }
            "getting-started" => Fragment {
                rows: vec![line("REPLACED-BY-PLUGIN".to_string(), Role::Body)],
            },
            // An id the guest does not know: empty, never a trap.
            _ => Fragment { rows: Vec::new() },
        }
    }
}

export!(Component);
