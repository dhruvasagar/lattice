# Demo media

Screenshots and motion clips for the README and the website.

## Motion comes from the demo video, not from VHS

**Decided 2026-09-21.** Clips for the site and README are cut from the
introductory demo video (`demo-script.md`) rather than rendered from
`.tape` files. One capture session produces both the video and its clips,
and nothing has to be installed to regenerate them.

That is a change of plan, and the reason is worth recording so nobody
re-litigates it: **VHS does not work on this machine.** It exits 0,
prints `Creating <name>.gif...`, and writes no file — silently, with
nothing on stderr. Diagnosed to headless Chrome:

```
ERROR:ui/display/mac/cv_display_link_mac.mm:195]
CVDisplayLinkCreateWithCGDisplay failed. CVReturn: -6670
```

VHS captures frames through Chrome DevTools' screencast API (vhs →
go-rod → Chrome). With the display link dead, screencast yields zero
frames, so there is nothing for `ffmpeg` to assemble and VHS gives up
without saying so. Reproduced with a six-line minimal tape in an empty
directory, on an interactive terminal — so it is not the tapes, not a
sandbox, and not a missing output directory. `ttyd` 1.7.7 and `ffmpeg`
9.0.2 are both present and are not implicated. Chrome was
153.0.8010.50.

If a later Chrome or VHS fixes that path, the tapes below still work —
they are declarative and keystroke-verified.

## The tapes are storyboards now

`tapes/{magit,buffers,config}.tape` stay in the repository. Every
keystroke sequence in them was verified end to end through `tmux` against
a real `lattice` binary, so they are the most reliable record of *how to
demonstrate each feature* — which is exactly what a video script needs.
`demo-script.md` draws on them. Read them as choreography, not as a build
step.

`config.tape` is the one worth reading before filming the config section:
it scaffolds a fresh config under a throwaway `$HOME` (via
`lattice --scaffold-init`), drops in `tapes/fixtures/config-demo-init.rs`
— a real `grammar`-seam component registering one programmatic
ex-command, `:hello <name>` — builds it for `wasm32-wasip2`, and cleans
up. It never touches a real `~/.config/lattice`. That fixture is the
annotated example in `docs/user/init.md`'s "Custom grammar" section.

`magit.tape` and `buffers.tape` run against **this repository** — a real
Rust project with real git history, which is what makes the magit
demonstration honest: staged hunks from an actual working tree rather
than a contrived fixture. `magit.tape` creates and removes its own
throwaway untracked file, so it neither depends on nor disturbs whatever
else is dirty.

## Screenshots are captured by hand

All of them, including the GPU-renderer shots — see
`screenshot-ideas.md` for the priority list, what each shot has to show,
and the resolution, theme and font conventions.

Org-mode and the agent buffers are hand-capture only for a further
reason: they need a fixture org file rather than a contributor's real
notes, the org plugin is not in the release's bundled core-plugin set,
and `:opencode` drives a real, non-deterministic model conversation that
no fixed timeline can script.

## Size budget

Anything that ships in the repository loads on the landing page and lands
in every clone. Keep screenshots under ~400 KB and any committed clip
under 4 MB. Prefer linking a hosted video over committing one.
