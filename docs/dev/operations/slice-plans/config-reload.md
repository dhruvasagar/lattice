# Whole-config reload — slice plan (CR)

> **Slice plan.** Sequencing, slice IDs, dependencies, status icons.
> Design contract: [`../../architecture/config-and-init.md`](../../architecture/config-and-init.md) §6a.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).
Every non-trivial slice ships the four artefacts (doc + bench where perf-relevant
+ tests including failure modes + graceful error handling).

**Status: 🚧 CR.0–CR.4 ✅; CR.5 📝.**

`:reload-config` re-read only `[plugins]` and `init.rs`. Edits anywhere else in
`lattice.toml` — options, the theme, `[lsp]`, `completion.per-language`,
`[llm]` — took a restart, and nothing said so. This plan makes the command
re-apply the whole configuration in boot's order, with a deleted line reverting
to its default.

| Slice | What | Crates | Status |
|---|---|---|---|
| CR.0 | Design (`config-and-init.md` §6a), `plugin-toml.md` corrected, this plan, ledger entry. No code. | docs | ✅ |
| CR.1 | The registry records the value each config file applied; `load_default_paths` reverts an option the files no longer mention when it still holds the file's value; `LoadOutcome::reverted`; the `ConfigFilesReloaded` event type. Bench: a reload case beside the existing load bench. | `lattice-config` | ✅ 6 tests; `config::reload_revert` 10 / 34 / 219 µs at 1 / 16 / 128 options |
| CR.2 | `:reload-config` emits `Effect::ReloadConfig`; the host reads the files off the editor thread, applies its caches on landing (theme, option cache, `[lsp]` tree, `completion.per-language` rebuilt from defaults), logs reverts and warnings, publishes `ConfigFilesReloaded`, then hands over to the loader for `[plugins]` + `init.rs`. Both renderers' effect classifiers. Test: the change is visible with no further keypress. | `lattice-grammar`, `lattice-plugin-loader`, `lattice-host`, `lattice-ui-tui`, `lattice-ui-gpui` | ✅ 6 tests (`reload_config_rereads_the_files`); the loader surface test updated |
| CR.3 | The LLM layer re-reads `[llm]` on `ConfigFilesReloaded`; running requests keep the config they started with; MCP servers keep running as started and the reload says so. | `lattice-ai` | ✅ 2 tests |
| CR.4 | LSP: running servers are told their settings changed (`workspace/didChangeConfiguration`) when the `[lsp]` tree differs. | `lattice-host` (the existing `LspConfigChanged` signal both peers already fan out) | ✅ 1 test |
| CR.5 | Startup-only settings that a reload cannot change are named in `*messages*`; user docs, `:help reload-config`, CHANGELOG, `BENCHMARKS.md`. | docs, owners of boot-only settings | 📝 |

## Sequencing

CR.1 → CR.2 → CR.3 → CR.4 → CR.5. CR.2 is the slice that makes the command
re-read the file; CR.3 and CR.4 are owners subscribing to its event and can
land in either order.

## CR.1 — what a file applied

- `ConfigRegistry` gains a map, canonical name → the option's formatted value
  right after a config file set it. `apply_assignment` and the composite path
  record into it on success; a later file overwrites an earlier one's entry
  (project over user, as the values themselves do).
- `load_default_paths` takes the previous map at the start, loads, then for
  each name no longer recorded: skip it if this load produced a failed
  assignment for it (the record carries over, unchanged); otherwise, if the
  option's current value equals the recorded one, reset it to its default
  (the `:set name&` path, so `OptionChanged` fires) and list it in
  `LoadOutcome::reverted`.
- Tests: deleted line reverts; a `:set` since is kept; a now-invalid line keeps
  the old value and stays recorded; project over user; alias spelling; boot
  (nothing previous) reverts nothing; a plugin option registered by then
  reverts too.

## CR.2 — the host runs the reload

- The file read moves off the editor thread (`spawn_blocking`); the outcome
  comes back through a channel drained in `run_tick_pending`, with the
  `async_landed` wake, so it lands without a keypress.
- `Editor::load_persistent_config` is split into read + apply; boot calls both
  in sequence, the reload calls apply on landing.
- `completion.per-language` is rebuilt from the built-in defaults, then the
  file, so a deleted section reverts too.
- Only after the host's apply does the loader reconcile `[plugins]` and rebuild
  `init.rs`, preserving boot's precedence.
