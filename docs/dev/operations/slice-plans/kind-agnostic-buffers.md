# Kind-agnostic buffer + mode infrastructure — slice plan

Sequencing companion to
[`docs/dev/architecture/kind-agnostic-buffers.md`](../../architecture/kind-agnostic-buffers.md).
The design fragment owns *what + why + contracts*; this file owns
*when + in what order + status*. Authoritative status per slice
lives in [`../implementation.md`](../implementation.md).

> **Status (2026-06-01):** H-series closed after H.2. H.1 + H.2 ✅; **H.3 deferred** to the WASM plugin host slice. See architecture fragment §10 for the rationale.

The H-series goal was to unblock the M.2.b.2 multibuffer major mode by making host's buffer + mode dispatch kind-agnostic. H.1 + H.2 achieve that for **in-tree extension crates** (the only producers that exist pre-v1). H.3 was the additional infrastructure needed by **WASM plugins** (producers that can't hold `&mut Editor`); pre-v1 it's premature.

| Slice | Title | Status | What lands |
|-------|-------|--------|------------|
| **H.1** | Generic `BufferStore::insert_document_buffer` API | ✅ 2026-05-31 (commit `22ee033`) | `BufferStore` trait (in `lattice-mode`) gained `insert_document_buffer(id, kind, handle: Arc<dyn Document>, flags, name)`. Host's `BufferRegistry` impl constructs the right `BufferData::Document` / `BufferData::Messages` / `BufferData::Multibuffer` variant from the `kind` tag. Added `BufferData::Multibuffer(DocumentEntry)` variant + matching `BufferEntry::kind()` arm. All `lattice-host` exhaustive matches over `BufferData` extended to cover Multibuffer. Kinds whose payload is NOT a Document (FileTree, Oil, Terminal, Help) keep their host-internal insertion path. Existing producers (synthetic_buffers, etc.) don't migrate; they keep using the host-internal path. |
| **H.2** | Mode declares target `BufferKind` | ✅ 2026-06-01 (commit `6f32f94`) | `Mode` trait gained `target_buffer_kind() -> Option<BufferKind>` (default `None`). `ModeRegistry::find_major_for_kind(BufferKind) -> Option<ModeId>` indexes registered modes by their declared kind. Overrides landed on `FileTreeMode → FileTree`, `OilMode → Oil`, `MarkdownMode → Help` (Option B: markdown-mode also serves `Document + Lang::Markdown` via the `Lang`-detection path; the two cohabit), `MessagesMode → Messages`, `TerminalMode → Terminal`. Host's `crate::modes::resolve_major_mode(®istry, kind, lang)` rewrites to use the lookup; the hardcoded match disappears. `BufferKind` gained `Hash`; `lattice-mode` gained a `tracing` dep for the first-registration-wins warning on clobbered kinds. |
| **H.3** | ~~Event-driven major-mode activation~~ | **Deferred** 2026-06-01 | Original design ([architecture §3 H.3](../../architecture/kind-agnostic-buffers.md)) preserved for the future WASM plugin host slice. See architecture §10 for the rationale. Summary: in-tree extension crates reach activation through the `ModeActivator` trait introduced in [`multibuffer-views.md`](multibuffer-views.md) §3.7; the event-bus path is the right shape for WASM plugins (capability gating, fuel-limited dispatch) and lands when that work begins. |

## What unblocks M.2.b.2

H.1 + H.2 are sufficient. M.2.b.2 design is locked at [`multibuffer-views.md`](multibuffer-views.md) §3.7 + the slice plan at [`slice-plans/multibuffer-views.md`](multibuffer-views.md). M.2.b.2 ships:

- `MultibufferMode` declaring `target_buffer_kind() = Some(BufferKind::Multibuffer)` (consumed by H.2's registry lookup).
- `lattice_multibuffer::create_multibuffer_view(activator, sources, excerpts, name, flags) -> BufferId` — inserts via H.1's `BufferStore::insert_document_buffer`, registers the typed handle in `MultibufferRegistry`, activates `multibuffer-mode` via the new `ModeActivator::activate_major_for_kind` (the in-tree synchronous activation surface that replaces the deferred H.3 event path).
- Host gains: one boot-time `register_multibuffer_modes` + `register InMemoryMultibufferRegistry` call. Zero `match BufferKind::Multibuffer` arms. Zero references to multibuffer types.

## Test discipline (H.1 + H.2 landed)

Both shipped with:

- Architecture-fragment updates + slice-plan status flips.
- Unit tests: H.1 verified `insert_document_buffer` round-trip through the `Document` slot; H.2 verified `find_major_for_kind` returns the registered id, returns `None` for unbound kinds, keeps the first registration on clobber, ignores modes that don't declare a target.
- Integration tests: `lattice-host` `modes::tests` rewrites confirm `resolve_major_mode` parity for Help / FileTree / Oil / Messages / Terminal post-H.2, and the empty-registry fall-through to `text-mode`.
- Graceful error handling: unknown-kind lookups return `Option::None`. Duplicate kind claims log + skip (first-wins).

## Resolved during H.1 / H.2

- **`BufferData::Multibuffer` unreachable-variant gap** (open issue from M.2.b.1) — H.1 closed it.
- **Pre-v1 `BufferData` extensibility** (architecture §8 Q1/Q2) — closed enum retained; in-tree extension crates use the typed `insert_document_buffer` primitive method; plugin-defined kinds revisit when the plugin host work starts.
- **H.3's right shape** — answered: not pre-v1. See architecture §10.
