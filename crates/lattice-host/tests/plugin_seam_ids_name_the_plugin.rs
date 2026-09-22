//! The host half of seam-id aliasing: the `PluginMetaRegistry` names a plugin
//! from any of its seam ids, still lists it once, and forgets every alias when
//! the plugin unloads.
//!
//! The loader half (that it reports the ids bindings are really stamped with)
//! is `lattice-plugin-loader/tests/seam_ids_resolve_to_the_plugin.rs`.

use lattice_core::Document as CoreDocument;
use lattice_host::editor::Editor;

fn editor() -> Editor {
    lattice_plugin_loader::disable_autoload();
    Editor::boot(CoreDocument::from_text("\n"))
}

#[test]
fn every_seam_id_names_the_plugin_and_it_is_listed_once() {
    let ed = editor();
    let sink = ed
        .services
        .get::<lattice_mode::PluginMetaSinkHandle>()
        .expect("the host publishes its meta registry as a sink");

    sink.register_plugin(900, "org".into(), String::new());
    sink.register_seam_ids(900, &[900, 901, 929]);

    for id in [900, 901, 929] {
        assert_eq!(
            ed.plugin_display_name(id).as_deref(),
            Some("org"),
            "seam id {id} names the plugin"
        );
    }
    assert_eq!(
        ed.loaded_plugins()
            .iter()
            .filter(|(_, m)| m.name == "org")
            .count(),
        1,
        "aliases are for lookup; `:list-plugins` still shows the plugin once"
    );

    sink.unregister_plugin(900);
    for id in [900, 901, 929] {
        assert_eq!(
            ed.plugin_display_name(id),
            None,
            "unload forgets seam id {id} too"
        );
    }
}
