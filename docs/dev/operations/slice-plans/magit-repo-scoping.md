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
| MR.1 | The resolver + the naming pair | 📝 |
| MR.2 | Per-buffer workdir record, written by the trigger | 📝 |
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

No behaviour change: nothing calls the resolver yet.

## MR.2 — the per-buffer record 📝

- A per-buffer workdir entry written by the trigger, read by the view at
  `on_activate` (the MG.26b blame-request shape — `on_activate` cannot
  see what the trigger saw).
- Cleared on `DocumentClosed`, like every other per-view magit entry.

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
