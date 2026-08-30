//! XF.4 — authorising a guest-returned effect's file paths.
//!
//! Design: [`cross-file-writes.md`](../../../docs/dev/architecture/cross-file-writes.md) §6.
//!
//! ## Why the check lives at the boundary
//!
//! `Effect::WriteToFile` writes into a file the editor may not have open. That
//! needs `fs:write` authority over the path, and the check has exactly one
//! place it can run.
//!
//! Not at the applier: by the time an effect reaches `Editor::handle_effect`
//! the host has no idea which plugin produced it, because effects from a
//! plugin and from a native mode are the same type — deliberately, since that
//! is what lets a mode drive an edit without the host growing a per-feature
//! `Action`. Threading a plugin id through the effect would mean putting
//! provenance inside guest-returned data, which is exactly what
//! `provenance_ids_are_host_issued_unique_and_stamp_the_plugin_layer` forbids.
//!
//! At the boundary the provenance is still known: the trampoline holds the
//! plugin's `Store<PluginState>`, and `PluginState::grant` is its effective
//! [`CapabilityGrant`]. So the conversion authorises, and an effect that
//! reaches the editor has already been checked.
//!
//! ## What a denial does
//!
//! Replaces the effect with an `Echo`, so the user is told rather than left
//! wondering why a key did nothing — the same reasoning as
//! `ProviderViewOutcome::Declined`. The rest of a `Many` is preserved: one
//! denied write must not silently cancel the other things an action did.

use std::path::{Path, PathBuf};

use lattice_grammar::{EchoLevel, Effect as NativeEffect};

use crate::capability::CapabilityGrant;

/// Authorises the file paths in a plugin's returned effects against its grant.
///
/// Built once per plugin at load — a grant never changes for a plugin's life,
/// so nothing here costs anything per keystroke beyond the prefix compare a
/// `WriteToFile` actually needs.
#[derive(Debug, Clone)]
pub struct EffectAuthorizer {
    /// Only the WRITE prefixes. A plugin granted `fs:read` over a tree may not
    /// write into it — `walk_within_grant` accepts read *or* write because a
    /// walk only reads, and this is the other half of that distinction.
    write_prefixes: Vec<PathBuf>,
    plugin: String,
}

impl EffectAuthorizer {
    pub fn new(grant: &CapabilityGrant, plugin: impl Into<String>) -> Self {
        Self {
            write_prefixes: grant
                .fs
                .iter()
                .filter(|g| g.write)
                .map(|g| g.prefix.clone())
                .collect(),
            plugin: plugin.into(),
        }
    }

    /// True when `path` lies within one of the plugin's `fs:write` prefixes.
    ///
    /// Both sides are canonicalized so a `..` segment cannot escape a granted
    /// prefix — the same rule `host_services::grant_permits_walk` applies, and
    /// for the same reason.
    ///
    /// **A target that does not exist yet canonicalizes its nearest real
    /// ANCESTOR.** Capture's first run creates its file, and a non-existent
    /// path canonicalizes to nothing; without this, "create the capture file"
    /// would be a permanent denial and the feature would be unreachable by
    /// design. Walking up rather than stopping at the immediate parent is what
    /// makes the same true when the directory is new too — see
    /// [`resolve_for_compare`].
    ///
    /// A path that still will not resolve falls back to its raw form, which
    /// requires a literal prefix match — so it can only ever deny more, never
    /// widen. Failing safe.
    pub fn permits_write(&self, path: &Path) -> bool {
        if self.write_prefixes.is_empty() {
            return false;
        }
        let real = resolve_for_compare(path);
        self.write_prefixes.iter().any(|prefix| {
            let canon_prefix = std::fs::canonicalize(prefix).unwrap_or_else(|_| prefix.clone());
            real.starts_with(&canon_prefix)
        })
    }

    /// Authorise an effect, replacing any unpermitted `WriteToFile` with an
    /// `Echo` naming the refusal.
    ///
    /// Recurses into `Many` so a write buried in a compound effect is checked
    /// too — an unchecked path there would be the whole gate, bypassed by
    /// wrapping.
    pub fn authorize(&self, effect: NativeEffect) -> NativeEffect {
        match effect {
            NativeEffect::Many(parts) => {
                NativeEffect::Many(parts.into_iter().map(|p| self.authorize(p)).collect())
            }
            NativeEffect::WriteToFile { ref path, .. } if !self.permits_write(path) => {
                // `info!`: one-shot and user-actionable ("a plugin was denied
                // fs access"), which is the level rule's own example — not the
                // per-keystroke `debug!` class.
                tracing::info!(
                    plugin = %self.plugin,
                    path = %path.display(),
                    "write-to-file denied: outside the plugin's fs:write grant"
                );
                NativeEffect::Echo {
                    level: EchoLevel::Warn,
                    text: format!(
                        "{}: write denied — {} is outside the plugin's granted paths",
                        self.plugin,
                        path.display()
                    ),
                }
            }
            other => other,
        }
    }
}

/// The path to compare against a granted prefix.
///
/// Canonicalize the target; if it does not exist, canonicalize the nearest
/// ancestor that DOES and re-attach the unresolved tail. Falls back to the raw
/// path when no ancestor resolves.
///
/// **Walking up all the way, not just one level.** Canonicalizing only the
/// immediate parent covers "the file is new in a directory that exists" and
/// silently fails "the file is new in a directory that is new too" — which is
/// every first write into a subdirectory a plugin owns: org-roam's
/// `daily/YYYY-MM-DD.org` on the day the journal starts, and capture's target
/// under a fresh folder. There the parent does not resolve either, the raw path
/// is used, and it is compared against a CANONICALIZED prefix — so on any
/// system where the grant sits behind a symlink the match fails and the write
/// is denied. macOS makes that the common case rather than the exotic one:
/// `/tmp` and `/var/folders` are both symlinks into `/private`, so a grant over
/// a temporary directory never matched a path this function had given up on.
///
/// The denial was indistinguishable from a capability the user had not granted
/// — the message names the path and says it is outside the granted paths, which
/// is exactly what it says when the grant really is missing.
///
/// Still fails safe: re-attaching an unresolved tail can only ever produce a
/// path at or below a real directory, and `..` inside the tail is normalised
/// away rather than followed, so a tail cannot climb back out of the prefix
/// that was just resolved.
fn resolve_for_compare(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    let mut tail: Vec<&std::ffi::OsStr> = Vec::new();
    let mut cursor = path;
    while let (Some(parent), Some(name)) = (cursor.parent(), cursor.file_name()) {
        tail.push(name);
        if let Ok(real_parent) = std::fs::canonicalize(parent) {
            let mut resolved = real_parent;
            for segment in tail.iter().rev() {
                // `.` contributes nothing and `..` pops — normalising here
                // rather than joining blindly is what keeps an unresolved tail
                // from escaping the ancestor it was resolved against.
                if *segment == std::ffi::OsStr::new("..") {
                    resolved.pop();
                } else if *segment != std::ffi::OsStr::new(".") {
                    resolved.push(segment);
                }
            }
            return resolved;
        }
        cursor = parent;
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::capability::FsGrant;

    fn grant(prefix: &Path, write: bool) -> CapabilityGrant {
        CapabilityGrant {
            fs: vec![FsGrant {
                prefix: prefix.to_path_buf(),
                write,
            }],
            ..Default::default()
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("lattice-xf4-{tag}-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_effect(path: &Path) -> NativeEffect {
        NativeEffect::WriteToFile {
            path: path.to_path_buf(),
            anchor: lattice_grammar::FileAnchor::End,
            text: "x\n".to_string(),
            cut: None,
            create_parents: false,
        }
    }

    #[test]
    fn a_path_inside_a_write_grant_is_permitted() {
        let dir = tmp("inside");
        let file = dir.join("archive.org");
        std::fs::write(&file, "").unwrap();

        let auth = EffectAuthorizer::new(&grant(&dir, true), "org");
        assert!(auth.permits_write(&file));
    }

    #[test]
    fn a_path_outside_every_prefix_is_refused() {
        let granted = tmp("outside-granted");
        let other = tmp("outside-other");
        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");
        assert!(!auth.permits_write(&other.join("archive.org")));
    }

    /// A READ grant is not a write grant. `walk_within_grant` accepts either
    /// because a walk only reads; this is the other half of that distinction,
    /// and conflating them would let any plugin that can list a tree write
    /// into it.
    #[test]
    fn a_read_only_grant_does_not_permit_writing() {
        let dir = tmp("readonly");
        let file = dir.join("archive.org");
        std::fs::write(&file, "").unwrap();

        let auth = EffectAuthorizer::new(&grant(&dir, false), "org");
        assert!(!auth.permits_write(&file));
    }

    #[test]
    fn a_plugin_with_no_fs_grant_writes_nothing() {
        let dir = tmp("nogrant");
        let file = dir.join("archive.org");
        std::fs::write(&file, "").unwrap();

        let auth = EffectAuthorizer::new(&CapabilityGrant::default(), "org");
        assert!(!auth.permits_write(&file));
    }

    /// The escape attempt. Without canonicalizing both sides, a path that
    /// *textually* starts with the prefix walks straight out of it.
    #[test]
    fn dotdot_cannot_escape_a_granted_prefix() {
        let base = tmp("escape");
        let granted = base.join("granted");
        let secret = base.join("secret");
        std::fs::create_dir_all(&granted).unwrap();
        std::fs::create_dir_all(&secret).unwrap();
        let target = secret.join("passwords");
        std::fs::write(&target, "").unwrap();

        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");

        let escaping = granted.join("..").join("secret").join("passwords");
        assert!(
            !auth.permits_write(&escaping),
            "`<granted>/../secret/passwords` resolves outside the grant and \
             must be refused — a textual prefix match would have allowed it"
        );
    }

    /// Capture's first run. The file does not exist, so it cannot be
    /// canonicalized; the PARENT is what decides. Without this the whole
    /// create-a-capture-file case would be permanently denied.
    #[test]
    fn a_not_yet_existing_file_is_judged_by_its_parent() {
        let dir = tmp("create");
        let missing = dir.join("capture.org");
        assert!(!missing.exists());

        let auth = EffectAuthorizer::new(&grant(&dir, true), "org");
        assert!(
            auth.permits_write(&missing),
            "a file this write would create must be permitted, or capture is \
             unreachable by construction"
        );
    }

    /// OR.10's first journal entry: the file is new AND so is the directory
    /// holding it, so neither the target nor its parent canonicalizes.
    ///
    /// Judging by the immediate parent alone fell back to the RAW path here and
    /// compared it against a canonicalized prefix — which on macOS never
    /// matches, because `std::env::temp_dir()` is a symlink into `/private`. So
    /// the very first `:org-roam-dailies-today` was denied with a message that
    /// reads exactly like a missing capability, and the second one (after the
    /// directory existed) worked.
    #[test]
    fn a_new_file_under_a_new_directory_is_judged_by_the_nearest_real_ancestor() {
        let dir = tmp("create-deep");
        let missing = dir.join("daily").join("2026-08-30.org");
        assert!(!missing.parent().unwrap().exists());

        let auth = EffectAuthorizer::new(&grant(&dir, true), "org");
        assert!(
            auth.permits_write(&missing),
            "a plugin writing the first file into a subdirectory it owns must \
             be permitted, or the feature is unreachable on its first use"
        );
    }

    /// …and walking up does not become a hole either. The tail is normalised,
    /// so `..` cannot climb back out of the ancestor it resolved against.
    #[test]
    fn an_unresolved_tail_cannot_escape_the_prefix_with_dotdot() {
        let granted = tmp("escape-granted");
        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");
        assert!(
            !auth.permits_write(&granted.join("new").join("..").join("..").join("stolen.org")),
            "a `..` in the unresolved tail must not widen the grant"
        );
    }

    /// …and the parent check does not become a hole: a non-existent file in a
    /// non-granted directory is still refused.
    #[test]
    fn a_not_yet_existing_file_outside_the_grant_is_still_refused() {
        let granted = tmp("create-granted");
        let other = tmp("create-other");
        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");
        assert!(!auth.permits_write(&other.join("capture.org")));
    }

    #[test]
    fn a_permitted_effect_passes_through_unchanged() {
        let dir = tmp("passthrough");
        let file = dir.join("a.org");
        std::fs::write(&file, "").unwrap();
        let auth = EffectAuthorizer::new(&grant(&dir, true), "org");

        assert!(matches!(
            auth.authorize(write_effect(&file)),
            NativeEffect::WriteToFile { .. }
        ));
    }

    #[test]
    fn a_denied_effect_becomes_an_echo_naming_the_plugin() {
        let granted = tmp("denied-granted");
        let other = tmp("denied-other");
        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");

        match auth.authorize(write_effect(&other.join("a.org"))) {
            NativeEffect::Echo { level, text } => {
                assert_eq!(level, EchoLevel::Warn);
                assert!(text.contains("org"), "names the plugin: {text}");
                assert!(text.contains("denied"), "{text}");
            }
            other => panic!("expected an Echo, got {other:?}"),
        }
    }

    /// A write buried inside a `Many` is checked too. Missing this would be
    /// the entire gate, bypassed by wrapping the effect in a list.
    #[test]
    fn a_write_nested_in_many_is_authorized_too() {
        let granted = tmp("nested-granted");
        let other = tmp("nested-other");
        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");

        let effect = NativeEffect::Many(vec![
            NativeEffect::None,
            NativeEffect::Many(vec![write_effect(&other.join("a.org"))]),
        ]);

        match auth.authorize(effect) {
            NativeEffect::Many(parts) => match &parts[1] {
                NativeEffect::Many(inner) => assert!(
                    matches!(inner[0], NativeEffect::Echo { .. }),
                    "a nested write must be refused, not passed through"
                ),
                other => panic!("expected a nested Many, got {other:?}"),
            },
            other => panic!("expected a Many, got {other:?}"),
        }
    }

    /// One denied write does not cancel the rest of a compound effect — the
    /// action's other work still happens, which is the graceful-degradation
    /// rule rather than an all-or-nothing refusal.
    #[test]
    fn the_rest_of_a_many_survives_a_denial() {
        let granted = tmp("survive-granted");
        let other = tmp("survive-other");
        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");

        let effect = NativeEffect::Many(vec![
            write_effect(&other.join("a.org")),
            NativeEffect::CursorMove(lattice_protocol::position::Position::new(3, 0)),
        ]);

        match auth.authorize(effect) {
            NativeEffect::Many(parts) => {
                assert!(matches!(parts[0], NativeEffect::Echo { .. }));
                assert!(
                    matches!(parts[1], NativeEffect::CursorMove(_)),
                    "the cursor move still happens"
                );
            }
            other => panic!("expected a Many, got {other:?}"),
        }
    }

    /// Effects with no path are untouched — the authorizer is about file
    /// writes and must not become a general filter.
    #[test]
    fn effects_without_a_path_are_left_alone() {
        let auth = EffectAuthorizer::new(&CapabilityGrant::default(), "org");
        assert!(matches!(
            auth.authorize(NativeEffect::None),
            NativeEffect::None
        ));
        assert!(matches!(
            auth.authorize(NativeEffect::Declined),
            NativeEffect::Declined
        ));
    }
}
