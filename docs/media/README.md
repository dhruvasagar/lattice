# Demo media

Screenshots and demo GIFs for the README and the website.

## Regenerating the TUI demos

The `.gif` files under `assets/media/demos/` are **generated**, not
recorded. Each has a declarative `.tape` beside it in `tapes/`:

```sh
brew install vhs
export PATH=/path/to/lattice-preview/bin:$PATH   # a built `lattice` on PATH
vhs docs/media/tapes/magit.tape     # writes assets/media/demos/magit.gif
```

Re-render them whenever the UI they show changes. A recorded video would
silently go stale; a tape fails loudly or shows the new UI.

Every tape starts with a `Require` line naming the binaries it needs
(`lattice`, and `cargo` for `config.tape`) — `vhs` refuses to run a tape
whose `Require`d binary is missing rather than recording a confusing
failure.

## The demo fixture

`magit.tape` and `buffers.tape` run against **this repository** — a real
Rust project with real git history, which is what makes the magit demo
honest (staged hunks from an actual working tree rather than a contrived
fixture). Before rendering, ensure the tree is clean (`git status`) and no
editor is already running. `magit.tape` creates and removes its own
throwaway untracked file for the hunk it stages, so it does not depend on —
or disturb — whatever else is dirty in your tree.

`config.tape` is fully self-contained: it scaffolds a fresh config under its
own throwaway `$HOME` (via `lattice --scaffold-init`), drops in
`tapes/fixtures/config-demo-init.rs` — a real `grammar`-seam component that
registers one programmatic ex-command, `:hello <name>` — builds it with
`cargo build --release --target wasm32-wasip2`, and cleans up its temp
directory afterward. It never touches a real `~/.config/lattice`. The
fixture source is exactly what `docs/user/init.md`'s "Custom grammar"
section walks through; see it there for the annotated version.

`org-agents.tape` does not exist yet — org-mode and agent buffers need a
fixture org file (not a contributor's real notes) and a way to show an agent
turn without a live, non-deterministic API call. See
`.superpowers/sdd/launch-0.9/task-L.4b-report.md` for the reasoning.

## GPUI shots

VHS drives a terminal, so it cannot capture the GPU renderer. Those shots
are captured by hand — see `screenshot-ideas.md`'s Technical Notes for the
resolution, theme and font conventions.

## GIF size budget

Demo GIFs ship in every clone and load on the landing page — keep each one
under 4 MB. If a render comes in larger, shorten the tape (fewer seconds of
`Show`n content) or reduce `Set Width` / `Set Height` rather than committing
an oversized file.
