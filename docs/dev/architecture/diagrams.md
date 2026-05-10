# Architecture diagrams

ASCII diagrams for the major lattice subsystems. Renders cleanly
in the in-editor `:help` view (which can't render Mermaid /
images) and in any markdown viewer. Optimised for "look at it
once, get the shape, then read the spec." For depth, follow the
links to `design.md` / `mode-architecture.md` / etc.

---

## 1. Three-layer architecture

The paramount goal is non-blocking: input (UI) and computation
(parsing, completion, LSP, plugin work) live in different layers
and communicate by typed messages.

```
        ┌─────────────────────────────────────────────────┐
        │                  UI LAYER                       │
        │            (single thread, ~120Hz)              │
        │                                                 │
        │   keystroke → input.rs → CommandInvocation      │
        │                              │                  │
        │   render ←── App ←─ effects ─┘                  │
        │     │                                           │
        │     └── ratatui paints frame                    │
        └────────────────────────┬────────────────────────┘
                                 │ typed events
                                 │ (publish_typed / subscribe_typed)
                                 ▼
        ┌─────────────────────────────────────────────────┐
        │                 CORE LAYER                      │
        │           (multi-thread, tokio)                 │
        │                                                 │
        │   • document actor (rope ops, snapshot)         │
        │   • event bus (typed pub/sub)                   │
        │   • config registry (typed options)             │
        │   • mode registry, keymap registry              │
        │   • LSP supervisor / actor / client             │
        │   • completion sources, snippet host            │
        └────────────────────────┬────────────────────────┘
                                 │ WIT calls (Component Model)
                                 ▼
        ┌─────────────────────────────────────────────────┐
        │              PLUGIN LAYER (Phase 7+)            │
        │      (one wasmtime::Store per plugin task)      │
        │                                                 │
        │   capability-gated, fuel-limited, isolated.     │
        │   user's init.rs compiled to WASM lives here.   │
        └─────────────────────────────────────────────────┘
```

**Read:** [`design.md`](design.md) §3 (architectural overview),
§5.5 (plugin subsystem), §5.7 (async runtime).

---

## 2. Threading model

```
   ┌─────────────────┐         ┌──────────────────────────┐
   │    UI thread    │         │   LSP runtime (tokio)    │
   │                 │         │                          │
   │  read keys      │         │  ┌──────────────────┐    │
   │       │         │  spawn  │  │  one task per    │    │
   │       ▼         ├────────►│  │  in-flight       │    │
   │  dispatch       │         │  │  request         │    │
   │       │         │         │  └──────────────────┘    │
   │       ▼         │         │                          │
   │  apply edits    │         │  ┌──────────────────┐    │
   │       │         │         │  │  attach_driver   │    │
   │       ▼         │         │  │  (DocumentOpened │    │
   │  render frame   │◄────────┤  │   → didOpen)     │    │
   │                 │  drain  │  └──────────────────┘    │
   │                 │  result │                          │
   └─────────────────┘         └──────────────────────────┘
        │                              ▲
        │                              │
        │   typed events on the bus    │
        ▼                              │
   ┌─────────────────────────────────────────────────────┐
   │                Event Bus (Arc<EventBus>)            │
   │   publish_typed::<T>(event) ─→ subscribe_typed::<T> │
   └─────────────────────────────────────────────────────┘
                          ▲
                          │
   ┌──────────────────────┴───────────────────────────────┐
   │             Plugin tasks (one per plugin)            │
   │      (each owns its own wasmtime::Store)             │
   └──────────────────────────────────────────────────────┘
```

UI thread does **no I/O, no parsing, no shaping**. Anything
expensive runs on the LSP runtime (`spawn_on_lsp_runtime`) or
the dedicated tokio thread pool. Results return via typed
channels; the UI thread drains on the next frame.

**Read:** [`design.md`](design.md) §5.7 (async runtime),
[`lsp-architecture.md`](lsp-architecture.md) §2 (supervisor /
actor split).

---

## 3. Buffer + pane model

Everything is a buffer (DESIGN.md §5.9). Documents, file trees,
oil listings, help docs, LSP logs all share one registry.

```
                ┌──────────────────────────────┐
                │       BufferRegistry         │
                │                              │
                │   BufferId ─→ BufferEntry    │
                │                              │
                │   • #1  doc:src/main.rs      │
                │   • #2  filetree:/src        │
                │   • #3  help:options         │
                │   • #4  lsp-log:rust         │
                │   • #5  oil:/Users/x/proj    │
                └──────────────────────────────┘
                          ▲
                          │ buffer_id
                          │
        ┌─────────────────┼─────────────────┐
        │                 │                 │
        ▼                 ▼                 ▼
  ┌──────────┐      ┌──────────┐      ┌──────────┐
  │  Pane A  │      │  Pane B  │      │  Pane C  │
  │  active  │      │          │      │          │
  │          │      │          │      │          │
  │ buf #1   │      │ buf #2   │      │ buf #3   │
  │ cursor   │      │ cursor   │      │ cursor   │
  │ scroll   │      │ scroll   │      │ scroll   │
  └──────────┘      └──────────┘      └──────────┘

Each pane carries its own (cursor, scroll). Same buffer can
appear in multiple panes; their cursors are independent.

       active_modes[buffer_id] = ActiveModes { major, minors }
       buffer_locals[buffer_id] = BufferLocals (typed map)

       resolved_options[buffer_id] = ResolvedOptions
            ├── modal-state layer
            ├── buffer-local layer
            ├── minor mode contributions (in activation order)
            ├── major mode contribution
            ├── typed-option (`:set`)
            └── default
```

`:bn` / `:bp` / `:ls` / `:bd` / `:b N` operate over the registry
uniformly; panes are the rendering surface, buffers are the
content.

**Read:** [`design.md`](design.md) §5.1 (buffer / document
model), §5.9 (UI components),
[`mode-architecture.md`](mode-architecture.md) §6.1 (option
resolution layer stack).

---

## 4. Mode resolution layer stack

For every option read on every buffer, the resolver walks the
layers highest-priority-first. The first non-empty layer wins.

```
   read: app.resolved_option::<Number>(buffer_id)
                    │
                    ▼
   ┌────────────────────────────────────────────────┐
   │  Layer 1: modal-state (M.7+ wires this)        │  ←  highest priority
   │           e.g. read-only when in Visual?       │
   ├────────────────────────────────────────────────┤
   │  Layer 2: buffer-local (`:setlocal`)           │
   │           per-buffer explicit overrides        │
   ├────────────────────────────────────────────────┤
   │  Layer 3: active minors (in activation order)  │
   │           last-activated wins on tie           │
   │           e.g. `lsp-mode` contributes nothing  │
   │                `line-numbers-mode` ⇒ Number=t  │
   ├────────────────────────────────────────────────┤
   │  Layer 4: active major                         │
   │           e.g. help-mode ⇒ ReadOnly=true       │
   ├────────────────────────────────────────────────┤
   │  Layer 5: typed-option (`:set`)                │
   │           the global default the user set      │
   ├────────────────────────────────────────────────┤
   │  Layer 6: built-in default                     │
   │           Number = true (etc.)                 │  ←  lowest priority
   └────────────────────────────────────────────────┘
                    │
                    ▼
              resolved value cached per buffer
              (rebuilt on mode toggle / `:set` write)
```

`:describe-option-resolution <name>` walks this stack at runtime
and shows which layer "won" — useful when a mode contribution
shadows a `:set` write.

**Read:** [`mode-architecture.md`](mode-architecture.md) §6.1 +
§6.3 (resolution mechanism + caching).

---

## 5. Event bus + typed events

```
                  ┌────────────────────────────┐
                  │      Arc<EventBus>         │
                  │                            │
                  │  by_kind:    HashMap...    │
                  │  by_typeid:  HashMap...    │
                  │  wildcard:   Vec<...>      │
                  └────────────────────────────┘
                       ▲                 │
                       │                 │
         publish_typed │                 │ subscribe_typed
                       │                 ▼
   ┌────────────────────┐         ┌──────────────────────┐
   │ Publisher:         │         │ Subscriber task:     │
   │                    │         │                      │
   │  bus.publish_typed │         │  loop {              │
   │   ::<LspBuffer-    │         │    let evt = rx      │
   │     Attached>(e)   │         │      .recv().await;  │
   │                    │         │    handle(evt);      │
   └────────────────────┘         │  }                   │
                                  └──────────────────────┘

Typed-event surface:

  • Event trait (event_registry.rs)
  • register_event! macro registers a descriptor
  • EVENT_DESCRIPTORS linkme slice (for :describe-events)
  • each event is a struct: LspBufferAttached, LspDocument-
    Changed, OptionChanged, MajorEntered, MinorActivated, ...

Source crates own their events:

  lattice-protocol  → core (DocumentOpened, etc.)
  lattice-lsp        → LspBuffer{Attached,Detached}, LspLog-
                       Pushed, LspDocumentChanged
  lattice-mode       → MajorEntered, MinorActivated, ...
```

**Read:** [`design.md`](design.md) §5.10 (event system + hooks).

---

## 6. LSP request lifecycle (hover example)

```
   user presses K (Normal mode)
        │
        ▼
   keymap_normal: K → Action::LspHoverRequest
        │
        ▼
   App::do_lsp_hover_request:
        ├── State A/B check: focused popup? → focus or no-op
        ├── cancel any in-flight hover token
        ├── M.6.2 sub-mode gate: check_lsp_sub_mode_gate(
        │      lsp-hover-mode, "lsp-hover-mode")
        │     │
        │     │ if umbrella off: echo "lsp-mode disabled"; bail
        │     │ if sub-mode off: echo "lsp-hover-mode disabled"; bail
        │     ▼
        ├── resolve buffer URI
        ├── spawn task on LSP runtime:
        │     ├── lsp.servers_for(uri) → Vec<ServerHandle>
        │     ├── for each handle (cancel-aware):
        │     │     if handle.capabilities().supports_hover():
        │     │       send hover request, await response
        │     │       send result on tx (mpsc channel)
        │     └── ...
        │
        ▼
   keystroke handler returns instantly. UI thread is free.
        │
        ▼ next frame's drain pump
   App::drain_pending_hover:
        ├── try_recv from rx
        ├── compose markdown body
        ├── App::do_open_hover (display_buffer with Hover category)
        │     → resolves to FloatingPopup(CursorAnchored) by
        │        default (or `hover.display` typed option)
        ▼
   render frame paints the popup
```

The cancel-stale-work step *precedes* the gate so gate state
flips also flip the relay. Token is checked at every async
boundary; a stale popup never lands over the new cursor.

**Read:**
[`lsp-architecture.md`](lsp-architecture.md) §7 (request
lifecycle),
[`mode-architecture.md`](mode-architecture.md) §M.6 (sub-mode
gating).

---

## 7. Insert completion pipeline

```
   user types in Insert mode
        │
        ▼
   InsertCompletion sources fire (gated on completion-mode):

   ┌──────────────────────┐  ┌──────────────────────┐
   │  gen:lsp-completion  │  │   gen:snippet         │
   │   (from lsp-actor)   │  │  (from snippet host)  │
   │   priority 200       │  │   priority 150        │
   └──────────┬───────────┘  └──────────┬────────────┘
              │                         │
              └────────────┬────────────┘
                           │
   ┌──────────────────────┐│   ┌──────────────────────┐
   │  gen:buffer-words    ││   │  gen:tree-sitter-     │
   │   (rope scan)        ││   │   symbol              │
   │   priority 100       ││   │   priority 80         │
   └──────────┬───────────┘│   └──────────┬────────────┘
              │            │              │
              └────────────┼──────────────┘
                           ▼
                ┌────────────────────┐
                │  per-language      │
                │  source filter     │
                │  (TOML / :set)     │
                └─────────┬──────────┘
                          ▼
                ┌────────────────────┐
                │   matcher + ranker │
                │  (priority + score)│
                └─────────┬──────────┘
                          ▼
                ┌────────────────────┐
                │   completion popup │
                │   (cursor-anchored)│
                └────────────────────┘

Selection in popup → accept (Tab / Enter) → insert candidate
                  → close popup → fire `OnTypeFormatting` for
                    a configured trigger char (M.6.2 gates on
                    `lsp-format-mode`).
```

**Read:**
[`insert-completion.md`](insert-completion.md) (the canonical
spec; sources, ranking, popup, ghost text, all here).

---

## See also

- The four paramount goals are stated in
  [`design.md`](design.md) §1.
- The mode-architecture migration plan +
  ledger of completed slices: [`mode-architecture.md`](mode-architecture.md) §10.
- LSP feature inventory + per-feature status:
  [`../notes/lsp-features.md`](../notes/lsp-features.md).
- Implementation status (what ships today):
  [`../operations/implementation.md`](../operations/implementation.md).
