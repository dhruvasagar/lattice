# Buffer-local options — slice plan

Sequencing companion to
[`docs/dev/architecture/buffer-local-options.md`](../../architecture/buffer-local-options.md).
Authoritative status per slice lives in
[`../implementation.md`](../implementation.md).

| Slice    | Title                                     | What lands |
|----------|-------------------------------------------|------------|
| **BL.1** ✅ | Write path: `:setlocal` + parse-without-write | `ErasedOption::parse_to_erased` — parses a value string to an erased `Arc<dyn Any>` without writing the global registry. `ConfigRegistry::parse_for_buffer_local(input) -> Result<(TypeId, Arc<dyn Any+Send+Sync>, canonical_name)>` — validates + returns the triple for an `OptionOverride` without touching global storage. `Editor::do_set_local(spec)` — looks up `buffer_local_overrides[active_buffer]`, pushes the parsed override, calls `recompute_options_for_buffer(active_buffer)` + `apply_option_cascade`. `:setlocal` / `:sl` ex-command wired to `do_set_local`. `:setlocal name&` clears one override; `:setlocal &` clears all. Tests: buffer-local value wins over global; different buffers get independent values; mode-contribution still beats buffer-local at `OverridePriority::High`; clear (`&`) reverts to global; `:scrollbind` set locally binds only the active buffer's pane. |
| **BL.2** ✅ | Query surface + `:setglobal`              | `OptionOrigin` enum in `lattice-config` (`Default`, `GlobalConfig`, `BufferLocal`, `ModeContribution { mode_id }`). `ResolvedOptions` entries carry origin alongside the erased value. Resolver writes the correct origin per layer. `:set name?` echoes `name=value  (origin)`. `:setlocal name?` echoes local or "not set locally (global: X)". `:setglobal [no]name[=value]` + `:sg` — writes global layer only, skips buffer-local. `:setglobal name?` returns global value ignoring locals. Tests: origin round-trips through resolver; `:set name?` shows `(global)` for a plain global set; shows `(buffer-local)` after `:setlocal`; shows `(mode: X)` when a mode contributes. |
| **BL.3** | `:describe-buffer` options section + `:options` view | `:describe-buffer` "Options" section lists all buffer-local overrides for the active buffer (canonical name + local value). `:options` ex-command opens a buffer-backed view (help-mode infrastructure) showing the full registered option catalog: effective value, global value, local value (if any), and origin for each. Tests: `:describe-buffer` shows local overrides; `:options` buffer contains all registered options. |

Slice sequencing: BL.1 before BL.2 (origin needs the write path to
test meaningfully); BL.2 before BL.3 (`:options` view uses origin
labels). BL.1 is self-contained and unblocks `:set scrollbind` vim
parity immediately.
