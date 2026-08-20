# Magit repo scoping — slice plan (MR)

> **Status: 🚧 Active.** Opened 2026-08-20. Implements
> [`magit-repo-scoping.md`](../../architecture/magit-repo-scoping.md):
> every magit surface acts on the repository containing the active
> buffer's file, not the process's working directory.

Design owns *what* and *why*; this file owns *when* and *in what order*.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred.

## Status

| Slice | Title | Status |
|---|---|---|
| MR.1 | The resolver + the naming pair — **lands with MR.2** | 📝 |
| MR.2 | Per-buffer workdir record + the first trigger (absorbs MR.1) | 📝 |
| MR.3 | Views read the record instead of cwd | 📝 |
| MR.4 | Action bodies read the buffer's repo (`magit_global_mode`, transients) | 📝 |
| MR.5 | The grep guard + docs | 📝 |

MR.1→MR.2→MR.3 is the spine. MR.4 is the half that makes it *correct*
rather than merely differently-wrong, so it is not optional polish.

---

## MR.1 — the resolver + the naming pair 📝

- `workdir::repo_for_trigger(…) -> Option<PathBuf>`, implementing design
  §2's three questions in order (magit buffer's own repo → active file's
  repo → cwd).
- `magit_buffer_name(view, workdir)` / `repo_display_from_name(name)` —
  one producer, one parser, per §3.1. Every caller through them; MG.15
  is the precedent for what a producer/parser split costs.
- Basename-collision qualification (`*magit:status:work/api*`).

**Tests.** Each resolution branch in isolation; a file outside any repo
falls through to cwd; two repos sharing a basename produce two distinct
names, and the same repo asked twice produces the same one (idempotent —
re-triggering must find the buffer you already have, not stack a second).

> **Corrected 2026-08-20, from attempting it: MR.1 cannot land alone.**
> Written and green (resolver + `repo_label` + `qualified_repo_label`, 10
> tests), it still could not be committed: with no caller yet, all three
> functions raise `dead_code`, and the standing rule treats a rustc
> warning in touched code as always-real rather than as noise to allow
> through. "No behaviour change, nothing calls it yet" is precisely the
> shape that rule refuses.
>
> So the boundary was wrong, not the code. **MR.1 lands as part of MR.2**,
> with the status trigger as its first caller. The work is preserved as a
> patch — `scratchpad/mr1-resolver.patch` in the session that wrote it —
> and reproducible from the test names above, which are the spec.
>
> Worth carrying forward as a slicing lesson: a slice whose whole content
> is "add a helper" has no warning-clean landing, so it is not a slice.
> Pair it with its first consumer.

## MR.2 — the per-buffer record + the first trigger 📝

Absorbs MR.1 (see the note above). Scope: **status only** — the other
views keep their fixed names and cwd resolution until MR.3, so this slice
stays landable.

- A workdir entry keyed by **buffer name**, written by the trigger and
  read by the view at `on_activate` (the shape
  `magit_diff_mode::ViewArgsRequests` and `magit_blame_mode::BlameRequests`
  already use — the opener leaves values under the buffer's name, the
  mode takes them when it activates). Keyed by name rather than id
  because the buffer does not exist yet when the trigger runs, and
  because `BufferStore::name_for` makes id → name → workdir a lookup
  rather than a second map to keep in sync.
- **Not** one-shot like `ViewArgsRequests`: MR.4's action bodies read it
  for the buffer's whole life. Cleared on `DocumentClosed`.

### The constraint that decides the mechanism (found 2026-08-20)

**An ex-command cannot reach the buffer store, but an action handler
can.** `lattice_grammar::ActionContext` (what an `ExCommandSpec::apply`
receives) carries `buffer_id` and a `Buffer`, but no path and no
services — deliberately, so `lattice-grammar` knows nothing about
services. And capturing the handle at registration does not work either:
`ServiceRegistry` is a plain `HashMap` (not `Arc`-shared, `register`
takes `&mut self`), and the host registers `BufferStoreHandle` at
`editor_boot.rs:1624` — *after* `lattice_magit::install` at `:630`.

So `C-x g` (which can be an action) and `:magit-status` (which cannot)
have different reach, and letting them diverge is not acceptable: the
request was explicitly that both behave the same.

**Chosen: magit caches the store handle in its own service.**
`MagitGlobalMode` is `ActivationPolicy::Universal` with an empty
`on_activate`, so it activates on the very first buffer at startup and
can stash `ctx.service::<BufferStoreHandle>()` into a magit-owned
`RepoScopes` service. Ex-command closures capture `RepoScopesHandle` at
boot — the `blame_requests` precedent, already threaded through
`register_ex_commands` for exactly this reason — and read the store at
call time. One resolution path, both surfaces.

Rejected alternatives, with reasons:

- **Let the two surfaces diverge** (`C-x g` resolves from the active
  buffer; `:magit-status` stays cwd-based unless given an argument).
  Rejected: it makes the common case the one that needs an argument, and
  the same command would mean two things depending on how it was reached.
- **A host-side `ModeContext::activated_from`** carrying the buffer the
  activation came from (the host already stashes it in
  `prev_pane_for_popup`). Genuinely elegant — no trigger plumbing at all,
  every view resolves at activation, both surfaces work for free. But it
  adds a generic host API for one consumer, makes resolution implicit,
  and is ambiguous for activations that are not triggers (`:b`, `<C-6>`).
  Worth revisiting only if a second subsystem wants the same fact.

### Then

- `:magit-status` resolves → computes the name → records the workdir →
  emits `OpenSyntheticBuffer` with the computed name.
- `magit-status-mode::on_activate` reads the workdir by its buffer name,
  falling back to `magit_workdir()` (a buffer reopened by `:b` after a
  restart has no record).
- The ~11 production sites naming `"*magit:status*"` (2 in
  `lattice-magit/src/lib.rs`, 3 in `magit_global_mode.rs`, 1 each in
  `magit_remote_mode.rs` / `magit_submodule_mode.rs`, 2 in
  `lattice-ui-tui/src/app/magit_bindings.rs`, 2 in
  `lattice-ui-tui/src/render.rs`) compute it instead, plus 4 in
  `lattice-host/tests/synthetic_buffer_survives_command_line.rs`.

**Tests.** The record survives the trigger→activation gap; closing the
buffer drops it; a second trigger for the same repo overwrites rather
than accumulating.

## MR.3 — views read the record 📝

Each view's `on_activate` takes its workdir from the record instead of
`magit_workdir()`, falling back to the resolver when there is no record
(a buffer opened by `:b` after a restart).

**Tests.** `C-x g` from a file in repo B opens repo B's status while the
editor's cwd is repo A — the acid test for the whole change, and the one
that fails today.

## MR.4 — action bodies 📝

The 14 `magit_workdir()` sites in `magit_global_mode.rs` plus the 3 in
`transients.rs` read **the buffer's** repo. Per design §4 this is what
separates "fixed" from "worse": a status buffer showing repo B whose `s`
stages into repo A is data-loss-shaped.

**Tests.** Stage / commit / checkout invoked in repo B's buffer touch
repo B, with the cwd pointed at repo A throughout.

## MR.5 — the guard + docs 📝

- Grep guard: no `magit_workdir()` outside the resolver (design §4's
  anti-rot rule), same shape as `gr_is_declared_once.rs`.
- User docs: `magit.md` on which repository a magit buffer acts on and
  what `:ls` now shows; the per-view pages where they name the buffer.
- `sync-docs.sh` + `zola build`.

---

## Cross-references

- [`../../architecture/magit-repo-scoping.md`](../../architecture/magit-repo-scoping.md) — design (what + why)
- [`magit.md`](magit.md) — the subsystem's own plan
