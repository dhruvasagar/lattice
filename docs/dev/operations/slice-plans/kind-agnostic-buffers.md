# Kind-agnostic buffer + mode infrastructure — slice plan

Sequencing companion to
[`docs/dev/architecture/kind-agnostic-buffers.md`](../../architecture/kind-agnostic-buffers.md).
The design fragment owns *what + why + contracts*; this file owns
*when + in what order + status*. Authoritative status per slice
lives in [`../implementation.md`](../implementation.md).

The H-series unblocks the M.2.b.2 multibuffer major mode by
making host's buffer + mode dispatch kind-agnostic. After the
H-series lands, **`lattice-multibuffer` (and every future
extension crate) can create a buffer of a new kind without
host knowing the kind exists.**

| Slice | Title | What lands |
|-------|-------|------------|
| **H.1** | 🗒 Generic `BufferStore::insert_buffer` API | `BufferStore` trait gains `insert_buffer(BufferEntry) -> BufferId`. `BufferEntry`, `BufferData`, `DocumentEntry`, `BufferFlags` hoist from `lattice-host::buffer_registry` to `lattice-mode` (with re-exports preserved in host so existing imports work). Add `BufferData::Multibuffer(DocumentEntry)` variant + matching `BufferEntry::kind()` arm. Existing host insertions don't migrate yet — they keep using `BufferRegistry::insert(...)` directly. Tests: extension-crate insertion roundtrips correctly; `kind()` returns `BufferKind::Multibuffer` for the new variant. |
| **H.2** | 🗒 Mode declares target `BufferKind` | `Mode` trait gains `target_buffer_kind() -> Option<BufferKind>` with default `None`. `ModeRegistry::find_major_for_kind(BufferKind) -> Option<ModeId>` indexes registered modes by their declared kind. Override on the existing major modes (`FileTreeMode`, `OilMode`, `MarkdownMode` for `Help`, `MessagesMode`, `TerminalMode`). Refactor host's `crate::modes::resolve_major_mode(kind, lang)` to use the registry lookup; the hardcoded `match kind { ... }` disappears (Document keeps its `Lang`-based dispatch). Tests: registry returns the correct ModeId for each registered kind; existing mode activation paths still work end-to-end. |
| **H.3** | 🗒 Event-driven major-mode activation | New `Event::BufferOpened { id: BufferId, kind: BufferKind }` in `lattice-protocol::event` (distinct from `DocumentOpened` which stays as the LSP-specific event with text payload). Host wires one `EventKind::BufferOpened` subscriber that looks up the kind via `BufferStore::kind_of(id)` and activates the major mode via `find_major_for_kind`. Migrate existing producers (`editor_boot`, `synthetic_buffers::ensure_named_document_for`, `dispatch::do_edit` brand-new-file branch, file-tree opener, oil opener) from direct `activate_major(...)` calls to `event_bus.publish(BufferOpened { ... })`. Integration test: each buffer kind constructed via the migrated path activates its major mode correctly. |

After H.3 lands, **M.2.b.2** resumes with:

- `MultibufferMode::target_buffer_kind() -> Some(BufferKind::Multibuffer)` declaration.
- `lattice_multibuffer::create_multibuffer_view(sources, excerpts) -> Result<Handle>` entry point that internally: builds handle → inserts into MultibufferRegistry → calls `BufferStore::insert_buffer(...)` → publishes `BufferOpened` event. Host has zero multibuffer-specific code.

## Slice sequencing

- **H.1** is foundational — H.2 and H.3 build on the registry / trait changes it lands.
- **H.2** depends on H.1 (`ModeRegistry::find_major_for_kind` needs the `BufferKind`-aware infrastructure that H.1 prepares around).
- **H.3** depends on H.2 (the subscriber's mode-lookup goes through `find_major_for_kind`).

## Test discipline

Each slice ships green-on-merge with:

- Architecture-fragment updates if the slice reveals a design refinement.
- Tests: new APIs + migration tests proving existing modes still activate correctly.
- Graceful error handling: unknown-kind lookups return `Option::None`, not panics. Producers handle "no major mode for this kind" without crashing.

## After H — back to multibuffer

- **M.2.b.2** ships `MultibufferMode` + `create_multibuffer_view(...)`. Uses H.1's `insert_buffer`, declares its target kind via H.2, publishes `BufferOpened` for H.3 to activate it.
- **M.2.b.3** ships the `]e` / `[e` / `]E` / `[E` motions through the mode's keymap.
- **M.2.c** ships the bench.

## Pending issue from M.2.b.1

M.2.b.1 added `BufferKind::Multibuffer` but did NOT add a matching `BufferData::Multibuffer(DocumentEntry)` variant. Today the variant is declared-but-unreachable — no path constructs an entry whose `kind()` returns `Multibuffer`. **H.1 fixes this** by adding the missing `BufferData::Multibuffer` variant alongside the trait method.
