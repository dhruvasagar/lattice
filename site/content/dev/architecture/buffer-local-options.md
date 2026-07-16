+++
title = "Buffer-local options"
+++

# Buffer-local options

Companion to `mode-architecture.md` §6 and `design.md` §5.12. Owns
the *what* and *why* of per-buffer option values; sequencing lives in
`docs/dev/operations/slice-plans/buffer-local-options.md`.

---

## 1. Problem

`:set` writes to the global config layer. Every buffer sees the same
value unless a mode contributes an `OptionOverrideSet`. There is no
user-facing way to say "wrap=true only in this buffer" without writing
a dedicated mode — the vim `:setlocal` / emacs `make-local-variable`
concept has no command surface yet.

The gap surfaced concretely in D.0b: `:set scrollbind=true` binds
**all** panes simultaneously because it writes the global layer; vim
users expect it to apply only to the current window.

---

## 2. Design choice: universal buffer-local layer (Option B)

No scope declaration per option. Every option is potentially
buffer-local. The user chooses `:set` (global) vs `:setlocal`
(current buffer) at call-site.

**Rationale:** The `options!` macro requires no change. Plugins and
modes can seed buffer-local values without touching the global
config. The origin system makes every effective value auditable
without a per-option taxonomy. Vim's `:setlocal` / `:setglobal`
vocabulary is preserved as the user-facing surface; the scope
semantics are determined by which command is used, not by a fixed
option attribute.

Rejected: per-option `#[scope(WindowLocal)]` declarations (Option A)
— creates a permanent per-option maintenance burden and the vim scope
taxonomy has known historical mistakes. Option C (hybrid with hints)
adds machinery over Option B with no capability gain for v1.

---

## 3. Resolution stack (updated)

Priority, highest to lowest:

```
1. Modal-state override      — e.g. Insert-mode forces wrap=false for
                               the command line buffer
2. Buffer-local explicit     — :setlocal foo=bar  ← this design
3. Active minor modes        — OverridePriority breaks ties
4. Major mode                — Mode::options() contribution
5. Global config             — :set foo=bar, init.toml, user TOML
6. Built-in default          — option's declared default value
```

Layers 1, 3, 4 already contributed via `OptionOverrideSet` passed
to `Resolver::resolve_into`. Layer 2 is now the
`buffer_local_overrides` map already stubbed on `Editor`; it feeds
the resolver as the second-highest layer. Layer 5 is the
`ConfigRegistry`. Layer 6 is bootstrapped once at startup.

---

## 4. Data model

### 4.1 `Editor::buffer_local_overrides`

```rust
pub buffer_local_overrides: HashMap<BufferId, OptionOverrideSet>,
```

Already declared on `Editor`. `recompute_options_for_buffer` already
reads it and passes it as layer 2 to the resolver. All that was
missing was the write path (`:setlocal`) and the parse-without-write
helper on `ErasedOption`.

### 4.2 `ErasedOption::parse_to_erased`

```rust
fn parse_to_erased(
    &self,
    value: &str,
) -> Result<Arc<dyn Any + Send + Sync>, String>;
```

Parses `value` against the option's `OptionType` and runs the
validator, but **does not write to the option's storage**. Returns the
typed value erased as `Arc<dyn Any + Send + Sync>`. Used by
`ConfigRegistry::parse_for_buffer_local` to produce an `OptionOverride`
without touching the global layer.

### 4.3 `ConfigRegistry::parse_for_buffer_local`

```rust
pub fn parse_for_buffer_local(
    &self,
    input: &str,
) -> Result<(TypeId, Arc<dyn Any + Send + Sync>, String), ConfigError>;
//           ^^^^^  ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^  ^^^^^^
//         type_id        erased parsed value        canonical name
```

Parses an option spec string (same syntax as `:set`), validates, and
returns the triple needed to construct an `OptionOverride` — without
touching the global registry. `ParsedSet::Query` returns
`ConfigError::QueryNotAllowed`; callers use `do_set_local` only for
writes.

### 4.4 `OptionOrigin`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptionOrigin {
    Default,
    GlobalConfig,                        // :set
    BufferLocal,                         // :setlocal
    ModeContribution { mode_id: String },
    // DirectoryLocal { path: PathBuf }, // future: .dir-locals.toml
    // FileLocal,                        // future: modeline
}
```

Lives in `lattice-config`. `ResolvedOptions` entries carry an
`OptionOrigin` alongside the erased value. The resolver writes
`GlobalConfig` when seeding from the registry bootstrap, `BufferLocal`
when writing from the buffer-local layer, and `ModeContribution`
when writing from a mode override.

---

## 5. Command surface

### `:set [no]name[=value]`

Unchanged. Writes to the global config layer.

### `:set name?`

Returns `name=<effective>  (<origin>)`. Examples:

```
scrollbind=true  (buffer-local)
wrap=false  (global)
tabstop=4  (mode: rust-mode)
```

### `:setlocal [no]name[=value]`  aliases: `:sl`

Writes to `buffer_local_overrides` for the active buffer. Triggers
`recompute_options_for_buffer` for that buffer and the normal
`apply_option_cascade` on the canonical name.

### `:setlocal name?`

Returns `name=<local>  (buffer-local)` if a local override exists, or
`name  not set locally (global: <global-value>)` if not.

### `:setglobal [no]name[=value]`  aliases: `:sg`

Writes to the global config layer only; does not touch the buffer-
local override for any buffer. Useful for changing the default without
overriding an existing local value.

### `:setglobal name?`

Returns the global config value regardless of any buffer-local
overrides.

### `:setlocal name&`

Clears the buffer-local override for `name`, reverting to the
next-lower layer. `:setlocal &` clears all local overrides for the
active buffer. Mirrors vim's `setlocal name&vim` syntax.

---

## 6. Cascade routing

`:setlocal` triggers `recompute_options_for_buffer(active_buffer)`
directly, then calls `apply_option_cascade` scoped to the active
buffer. `:set` continues the current global-broadcast behaviour (all
buffers' resolved caches refreshed). This is correct: a global change
can affect any buffer; a buffer-local change only affects that buffer.

No changes to `drain_option_changes` plumbing are needed; the scope
distinction is achieved by calling per-buffer vs global recompute.

---

## 7. Traceability

`:set option?` shows effective value + origin in parens.
`:setlocal option?` shows the local value or "not set locally (global:
X)". `:describe-buffer` lists all buffer-local overrides with
canonical names. Future `:options` view (BL.3) shows the full catalog
with effective, global, local, and origin for every registered option.

---

## 8. Interaction with modes

Mode contributions (layers 3/4) win over `:setlocal` (layer 2). A
mode contributing `ReadOnly = true` with `OverridePriority::High`
cannot be overridden by `:setlocal read-only=false`. For non-critical
`Normal`-priority contributions, a future high-priority buffer-local
override could force through; BL.1 does not expose priority in
`:setlocal` — all writes use `Normal`.
