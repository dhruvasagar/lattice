# Config & init lifecycle — slice plan

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/config-and-init.md`](../../architecture/config-and-init.md).
> Reworks the init.rs lifecycle to init-first + event-deferred plugin config, adds
> the plugin-lifecycle event, and the available-vs-enabled minor-mode model. First
> consumer: auto-pair (off-by-default, enabled via an `on-plugin-loaded` handler) —
> supersedes the abandoned `enable-mode`-at-top-level / pending-value-store plans.

Status icons: ✅ done · 🚧 in progress · 📝 planned. Every non-trivial slice ships
the four artefacts (doc + bench-where-perf-relevant + test incl. failure modes +
graceful error handling).

**Status: ✅ CI.1–CI.7 complete.** The full config/init lifecycle ships: the
`plugin-loaded` event, init-first ordering, the available-vs-enabled gate,
`enable-mode` + open-buffer re-activation, auto-pair off-by-default enabled via
init.rs, and the init.md rewrite.

## Sequencing

```
CI.1 plugin-loaded event ──┬─► CI.2 init-first ordering ──┐
                           │                              │
CI.3 mode available/enabled ─► CI.4 enable-mode + reactivate ─┴─► CI.5 auto-pair off + init.rs enables it
                                                                    │
                                                              CI.6 init.md rewrite (post-impl)
```

CI.1 (the event) and CI.3 (the enablement gate) are independent foundations. CI.2
(ordering) needs CI.1's event to matter. CI.4 (the seam + re-activation) needs
CI.3. CI.5 ties it together end-to-end through the loaded auto-pair plugin. CI.6
documents the settled surface (after implementation, per the standing request).

## Slices

### CI.1 — the `plugin-loaded` / `plugin-unloaded` event  ✅
Mirror `Event::PluginCrashed`: add `Event::PluginLoaded { name, id }` +
`Event::PluginUnloaded { name, id }` (enum + `EventKind` + the WIT `event` /
`event-kind` mirror + boundary projection) and a `plugin-loaded` events-seam
subscription filter. The loader publishes `plugin-loaded` at `load_discovered`
completion (**after** the full drain — every seam registered), `plugin-unloaded`
after teardown reverses a plugin's contributions. **Exit:** a fixture events-guest
subscribed to `plugin-loaded` receives one event, carrying the loaded plugin's
name, after that plugin finishes loading; unload fires `plugin-unloaded`. Test:
subscribe → load a second plugin → assert the event; the crash/teardown path.
Graceful: a subscriber that traps is quarantined, never blocks the publish.

### CI.2 — init.rs loads first  ✅
Reorder `lattice_plugin_loader::install`: load `init.rs` (`<config>/lattice/init/`)
and await it (so its subscriptions register) **before** discovering + loading the
`<config>/lattice/plugins/` tree. `lattice.toml` applies before init.rs (§3
ordering). **Exit:** an init.rs that subscribes to `plugin-loaded` for a plugin
discovered afterwards receives the event (subscription was in place first); a
regression test pins the order. Graceful: an absent/failed init.rs degrades to
"no user config", never blocks plugin discovery (the existing `load_path` skip).

### CI.3 — minor-mode available vs enabled  ✅
`ModeRegistry` gains an enablement gate: `auto_activatable_minors(major, kind)`
returns a minor iff `admits(...) AND enabled(id)`. `register` (native) auto-enables;
new `register_available` (used by `register_plugin_mode`, mode_host.rs) registers
**without** enabling. Add `set_minor_enabled(id, bool)`. **Exit:** a native minor
(policy `Global`) still auto-activates; a plugin minor (policy `Global`) is inert
until enabled; unit tests cover both + the gate. No hot-path bench (activation is a
rare per-`MajorEntered` event, O(registered minors)). This is a `lattice-mode`
change; the RCU handle already supports clone-mutate-store.

### CI.4 — `enable-mode` / `disable-mode` seam + re-activation  ✅
`wit/modes.wit` gains `enable-mode(id)` / `disable-mode(id)`; the host body
RCU-flips `set_minor_enabled` and **re-activates open buffers**: for each open
buffer whose major admits the now-enabled mode, activate it (bounded, O(open
buffers)); disable deactivates. The re-activation is the one real piece of
machinery — reused by a future `:mode-enable` and `:reload-config`. **Exit:** an
open document buffer gains an enabled plugin minor's keymap without reopening;
disable removes it; a mode not admitted by a buffer's major is untouched. Test:
enable → assert the mode active + its gated keymap resolves on the open buffer;
disable → gone. Graceful: enabling an unknown/duplicate id logs + no-ops.

### CI.5 — auto-pair off-by-default, enabled from init.rs  ✅
Flip auto-pair's `auto-pair-mode` to **available-but-off** (it already declares
`Global` scope; CI.3 makes that inert until enabled — the plugin needs no change
beyond confirming it doesn't self-enable). Author the reference/example init.rs
that `on_plugin_loaded("auto-pair")` → `enable_mode("auto-pair-mode")`. **Exit:**
end-to-end through the real loader (the CI.2 ordering + CI.1 event + CI.4 seam): a
booted editor with that init.rs has auto-pair-mode active on a document buffer
*after* auto-pair loads; without the init.rs line, auto-pair is loaded but inert.
Test: the loader harness (auto_pair.rs precedent) with an init.rs fixture wired
ahead of the auto-pair plugin. **Milestone: user-controlled, event-deferred plugin
config working end to end.**

### CI.7 — `config.set-option` (the missing value-setting front-end)  ✅
Audit-driven (an init.md accuracy pass found the annotated example calling a
`set-option` that didn't exist): the config seam had `register-option` (declare) +
`get-option` (read) but **no way to SET a value**, so `init.rs` couldn't override
options — a crippled config front-end contradicting §5's "two value-setting
front-ends". Added `config.set-option(name, value) -> bool`, a thin wrapper over
`ConfigRegistry::parse_and_set_command` (the `:set name=value` path: coerce +
validate + publish `OptionChanged`); `false` on unknown option / invalid value /
no registry. **Landed:** `config-guest` now `set-option`s a registered option and
reads back the changed value (`config_source.rs`); init.md + the design fragment
(§5, §9) corrected to the real API. This is what makes an `init.rs`
`on-plugin-loaded` handler able to *configure* a plugin's options, not only enable
its mode.

### CI.6 — init.md rewrite  ✅
Rewrite `docs/user/init.md` for the settled model: the boot ordering (init-first),
immediate-vs-deferred config, the `on-plugin-loaded` pattern, `enable-mode`, and a
**large annotated example init.rs** with labeled sections — keybindings, option
overrides, a custom ex-command, enabling a plugin minor mode, and several
event-based flows (`on BufferOpened → set options by filetype`, `on plugin-loaded →
configure the plugin`, …), each explaining *why the handler calls an API, not a `:`
command string*. Correct the `~/.config` paths (already fixed in code). **Exit:**
init.md documents only surfaces that exist post-CI.5; the annotated example
compiles conceptually against the real seams. (Doc slice — no code/test.)

## Notes

- **Supersedes** the earlier mid-design plans captured in
  [`plugin-auto-pair.md`](plugin-auto-pair.md) discussion (the `enable-mode`
  imperative-at-top-level + the re-activation event + the pending-value store).
  The event-deferred model replaces all three; auto-pair's AP.x slices are
  unaffected except that AP.4 (bundle) now ships the mode off-by-default.
- **Cross-renderer:** none of these touch the renderer; mode activation +
  keymap-layer changes are renderer-agnostic (the buffer substrate already
  fans the signals).
- **Deferred:** full deferred-binding for *imperative* refs to third-party plugins
  loaded at runtime via `:plugin-load` (beyond the boot `plugin-loaded` handlers) —
  the same event primitive covers it, but the runtime-install UX is a later slice.
