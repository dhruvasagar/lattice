//! The `theme` guest→host element-declaration seam (TC.4).
//!
//! A theme-contributing plugin implements the `theme-plugin` world: it
//! **imports** the `theme` API (`register-element`) and **exports**
//! `register-theme-elements`, which the host calls once to drive declaration.
//! This module holds the `bindgen!` for that world plus the host-side
//! conversion from the WIT `style-spec` to a native
//! [`StyleSpec`](lattice_theme::StyleSpec), factored out so it is unit-testable
//! without a `Store` (the `config_host` precedent).
//!
//! **The canonical API is the WIT** (`theme.wit`) — any component-model
//! language calls `register-element` directly. A plugin element lands in the
//! SAME registry builtins use, so themes override it, `:customize` edits it and
//! `:describe-element` documents it with NO host kind-branch.
//!
//! This closes `theme-system.md`'s deferred WIT-registration item, which was
//! designed there and waited for a real consumer.

use lattice_theme::{ColorRef, ElementName, ElementOwner, ModifierSet, StyleSpec, ThemeRegistry};

use crate::{
    Component, PluginBudget, PluginHost, PluginHostError, PluginManifest, TrustTier, arm_store,
    classify_trap,
};

pub(crate) mod bindings {
    wasmtime::component::bindgen!({
        world: "theme-plugin",
        path: "../../wit",
        // Wired into the same async linker as WASI + the `theme` host funcs, so
        // the export is async (the `config-plugin` precedent). Registration is
        // off every hot path, so async costs nothing.
        exports: { default: async },
        with: {
            "lattice:plugin-host/logging": crate::lattice::plugin_host::logging,
        },
    });
}

use bindings::lattice::plugin_host::theme as wit;

/// Convert a WIT colour reference to the native one.
///
/// `literal-rgb` crosses as a packed `0xRRGGBB` rather than three bytes: it is
/// the form every other colour in the boundary already uses (`VirtualRow::bg`,
/// the `ui` mirror), so a plugin that computed a colour once can pass it
/// everywhere without repacking.
fn color_ref_from_wit(c: wit::ColorRef) -> ColorRef {
    match c {
        wit::ColorRef::Palette(key) => ColorRef::Palette(key.into()),
        wit::ColorRef::LiteralRgb(rgb) => ColorRef::Literal(lattice_theme::Color::Rgb(
            ((rgb >> 16) & 0xff) as u8,
            ((rgb >> 8) & 0xff) as u8,
            (rgb & 0xff) as u8,
        )),
        wit::ColorRef::Default => ColorRef::Default,
    }
}

/// Convert a WIT `style-spec` to the native [`StyleSpec`].
///
/// Total — every WIT shape maps, so there is no failure mode here and the
/// `register-element` `err` is reserved for registry-level rejections. `family`
/// and `weight` have no WIT counterpart by design (see `theme.wit`), so they
/// stay `None`; a plugin cannot set them, which is the intended surface rather
/// than a lossy conversion.
pub fn style_spec_from_wit(spec: wit::StyleSpec) -> StyleSpec {
    StyleSpec {
        inherit: spec.inherit.map(ElementName::from),
        fg: spec.fg.map(color_ref_from_wit),
        bg: spec.bg.map(color_ref_from_wit),
        modifiers: ModifierSet {
            bold: spec.modifiers.bold,
            italic: spec.modifiers.italic,
            underline: spec.modifiers.underline,
            dim: spec.modifiers.dim,
            reverse: spec.modifiers.reverse,
        },
        scale: spec.scale,
        family: None,
        weight: None,
    }
}

/// The `register-element` host-service body. Registers into the SAME registry
/// builtins use, owned by the plugin so unload can reverse it.
///
/// Returns the registered (namespaced) name so the caller can record a teardown
/// token.
pub fn register_plugin_element(
    registry: &dyn ThemeRegistry,
    plugin_id: &str,
    name: &str,
    doc: &str,
    spec: StyleSpec,
) -> String {
    let full = format!("{plugin_id}.{name}");
    // `doc` must be `&'static str` for the registry, and a plugin's doc is
    // runtime data — leak it. Bounded by the element count of the loaded
    // plugins (tens), declared once at load, so this is a one-time cost per
    // element rather than a growing leak; the alternative is widening the
    // native registry's doc field for a case only plugins have.
    let doc: &'static str = Box::leak(doc.to_string().into_boxed_str());
    registry.register(
        ElementName::from(full.clone()),
        ElementOwner::Plugin(plugin_id.to_string().into()),
        spec,
        doc,
    );
    full
}

/// TK.5: the `set-element-override` body — an override for an element this
/// plugin owns, above the theme.
///
/// **Ownership is checked, not assumed.** Namespacing already bounds what a
/// plugin can name, so this check should be unreachable; it exists because
/// "should be unreachable" is exactly the reasoning that makes a security
/// boundary depend on a call site staying correct. Refusing here means a
/// future caller that forgets to namespace is refused rather than allowed to
/// restyle a builtin.
pub fn set_plugin_element_override(
    registry: &dyn ThemeRegistry,
    plugin_id: &str,
    name: &str,
    spec: StyleSpec,
) -> Result<(), String> {
    let full = format!("{plugin_id}.{name}");
    let element = ElementName::from(full.clone());
    let Some(info) = registry.describe(&element) else {
        // A typo is a named refusal rather than an override that lands
        // nowhere and looks like the feature not working.
        return Err(format!(
            "set-element-override: `{full}` is not a registered element"
        ));
    };
    match &info.owner {
        ElementOwner::Plugin(owner) if owner.as_ref() == plugin_id => {}
        _ => {
            return Err(format!(
                "set-element-override: `{full}` is not owned by `{plugin_id}`"
            ));
        }
    }
    registry.set_override(element, spec);
    Ok(())
}

impl PluginHost {
    /// Instantiate a `theme-plugin` component under its capability grant, drive
    /// its `register-theme-elements` export once, and return the host-issued id
    /// plus the element names it registered (the teardown tokens).
    ///
    /// Mirror of [`spawn_config_plugin`](Self::spawn_config_plugin): the
    /// registry is wired onto `PluginState` BEFORE the export runs so the
    /// guest's imported `register-element` reaches it.
    pub async fn spawn_theme_plugin(
        &self,
        component: &Component,
        manifest: &PluginManifest,
        tier: TrustTier,
        budget: PluginBudget,
        registry: &lattice_theme::ThemeRegistryHandle,
    ) -> Result<(crate::PluginId, Vec<String>), PluginHostError> {
        let (wasi, outcome, _data_dir) = self.build_plugin_wasi(manifest, tier);
        for denied in &outcome.denied {
            tracing::warn!(
                plugin = %manifest.id,
                capability = ?denied,
                "theme plugin loaded with a withheld capability (reduced function)"
            );
        }
        let mut store = self.new_store(wasi, outcome.grant, budget, Some(&manifest.id))?;
        let bindings =
            bindings::ThemePlugin::instantiate_async(&mut store, component, &self.linker)
                .await
                .map_err(|e| PluginHostError::Instantiate(e.into()))?;

        let id = self.alloc_id();
        store.data_mut().theme_registry = Some(registry.clone());
        store.data_mut().log_ctx = self.log_ctx_for(id);

        arm_store(&mut store, budget)?;
        bindings
            .call_register_theme_elements(&mut store)
            .await
            .map_err(|source| PluginHostError::Trap {
                func: "register-theme-elements",
                kind: classify_trap(&source),
                source: source.into(),
            })?;

        let elements = std::mem::take(&mut store.data_mut().theme_contributions);
        Ok((id, elements))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    #[test]
    fn a_palette_reference_survives_the_crossing() {
        let spec = style_spec_from_wit(wit::StyleSpec {
            inherit: Some("listing.file".to_string()),
            fg: Some(wit::ColorRef::Palette("blue".to_string())),
            bg: None,
            modifiers: wit::ModifierSet {
                bold: None,
                italic: None,
                underline: None,
                dim: None,
                reverse: None,
            },
            scale: None,
        });
        assert_eq!(spec.inherit, Some(ElementName::from("listing.file")));
        // A palette KEY, not a resolved colour — this is what makes a plugin's
        // element re-colour on `:colorscheme`.
        assert!(matches!(spec.fg, Some(ColorRef::Palette(_))));
    }

    #[test]
    fn a_literal_rgb_unpacks_to_the_right_channels() {
        let spec = style_spec_from_wit(wit::StyleSpec {
            inherit: None,
            fg: Some(wit::ColorRef::LiteralRgb(0x11_22_33)),
            bg: Some(wit::ColorRef::Default),
            modifiers: wit::ModifierSet {
                bold: None,
                italic: None,
                underline: None,
                dim: None,
                reverse: None,
            },
            scale: None,
        });
        // Channel order is the one place a packed colour can silently invert.
        assert!(matches!(
            spec.fg,
            Some(ColorRef::Literal(lattice_theme::Color::Rgb(
                0x11, 0x22, 0x33
            )))
        ));
        assert!(matches!(spec.bg, Some(ColorRef::Default)));
    }

    #[test]
    fn modifiers_keep_their_three_states() {
        let spec = style_spec_from_wit(wit::StyleSpec {
            inherit: None,
            fg: None,
            bg: None,
            modifiers: wit::ModifierSet {
                bold: Some(true),
                // `Some(false)` must survive as "clear an inherited bold",
                // NOT collapse to `None` (unspecified). Emacs faces
                // distinguish the two and so does the resolver.
                italic: Some(false),
                underline: None,
                dim: None,
                reverse: None,
            },
            scale: Some(1.5),
        });
        assert_eq!(spec.modifiers.bold, Some(true));
        assert_eq!(spec.modifiers.italic, Some(false));
        assert_eq!(spec.modifiers.underline, None);
        assert_eq!(spec.scale, Some(1.5));
    }

    #[test]
    fn registration_namespaces_by_plugin_id() {
        let reg = lattice_theme::InMemoryThemeRegistry::new(lattice_theme::default_palette());
        let full = register_plugin_element(
            &reg,
            "treesitter-context",
            "background",
            "The context strip backdrop.",
            StyleSpec::new().fg(ColorRef::Palette("overlay".into())),
        );
        assert_eq!(full, "treesitter-context.background");
        assert!(
            reg.id(&ElementName::from("treesitter-context.background"))
                .is_some(),
            "the namespaced element is registered, so a theme can override it \
             and `:customize` can list it"
        );
        // Unnamespaced must NOT exist — a plugin cannot squat a bare name.
        assert!(reg.id(&ElementName::from("background")).is_none());
    }
}

#[cfg(test)]
mod tk5_tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use lattice_theme::{ColorRef, InMemoryThemeRegistry, StyleSpec, default_palette};

    fn rig() -> InMemoryThemeRegistry {
        // The POPULATED palette, not `Palette::default()` — an empty one
        // has no `yellow` or `orange`, so every palette reference resolves
        // to `None` and an override would be indistinguishable from a
        // default. The first version of this test asserted `None != None`.
        let r = InMemoryThemeRegistry::new(default_palette());
        register_plugin_element(
            &r,
            "org",
            "todo.WAITING",
            "a TODO state",
            StyleSpec {
                fg: Some(ColorRef::Palette("yellow".into())),
                ..Default::default()
            },
        );
        r
    }

    /// The point of the slice: an override BEATS the element's default,
    /// which is what a default alone could never express — a default sits
    /// below the active theme, so a plugin could not say "the user
    /// configured this and it must win".
    #[test]
    fn tk5_an_override_beats_the_registered_default() {
        let r = rig();
        let name = ElementName::from("org.todo.WAITING".to_string());
        let before = r.resolved().get(r.id(&name).unwrap()).fg;

        set_plugin_element_override(
            &r,
            "org",
            "todo.WAITING",
            StyleSpec {
                fg: Some(ColorRef::Palette("orange".into())),
                ..Default::default()
            },
        )
        .expect("the plugin owns this element");

        let after = r.resolved().get(r.id(&name).unwrap()).fg;
        assert_ne!(before, after, "the override must change the resolved style");
    }

    /// Namespacing already bounds what a plugin can name, so this refusal
    /// should be unreachable. It is checked anyway, because "should be
    /// unreachable" is exactly the reasoning that makes a boundary depend on
    /// every call site staying correct.
    #[test]
    fn tk5_a_plugin_cannot_override_an_element_it_does_not_own() {
        let r = rig();
        r.register(
            ElementName::from_static("pane.separator"),
            ElementOwner::Core,
            StyleSpec::default(),
            "a builtin",
        );
        // Reaching past the namespace, as a miswired caller would.
        let err = set_plugin_element_override(&r, "pane", "separator", StyleSpec::default())
            .expect_err("a builtin is not the plugin's to restyle");
        assert!(err.contains("not owned by"), "{err}");
    }

    /// A typo must be a named refusal rather than an override that lands
    /// nowhere and reads as the feature not working.
    #[test]
    fn tk5_overriding_an_unregistered_element_says_so() {
        let r = rig();
        let err = set_plugin_element_override(&r, "org", "todo.NOPE", StyleSpec::default())
            .expect_err("no such element");
        assert!(err.contains("not a registered element"), "{err}");
    }
}
