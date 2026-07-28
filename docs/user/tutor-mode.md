---
summary: "The interactive tutor: a gamified lesson sequence (lives, score, high score) that teaches lattice from inside lattice."
related: [modal-editing, modes, help]
---

# tutor-mode

The **tutor** is an interactive, gamified lesson sequence — vim's `vimtutor`
crossed with an arcade scoreboard. You learn lattice *inside* lattice: each
lesson opens as a real editable buffer, you do the exercise on a live practice
line, and a sticky HUD row at the top tracks lives, score, and your high score.

## Quick reference

| You want…                        | Do this                                            |
|----------------------------------|----------------------------------------------------|
| Start the tutor (lesson 1)       | `:tutor`                                            |
| Start a specific lesson          | `:tutor 5`                                          |
| Advance to the next exercise     | `<CR>` (or `<C-j>`, or `:tutor-next`)              |
| Go back one exercise             | `<C-k>` (or `:tutor-prev`)                          |
| Retry after a GAME OVER          | `<C-k>` (reloads the lesson)                        |

## Starting

```
:tutor          " lesson 1
:tutor 6        " jump straight to lesson 6
```

`:Tutor` is an alias. The lesson opens in a normal buffer (you can edit, move,
undo like any file) with the tutor HUD pinned above the first line.

The seven lessons:

| # | Title |
|---|-------|
| 1 | Basic motions |
| 2 | The grammar (operators · motions · text objects) |
| 3 | Visual mode, registers, search, macros |
| 4 | The mode system and emacs-style help |
| 5 | Splits, buffers, project search, diff, LSP |
| 6 | Advanced editing (tree-sitter text objects, narrow, folding, soft-wrap) |
| 7 | Customization (`:colorscheme`, `:set`, `:customize`, the modeline) |

## Playing an exercise

Each exercise marks a **practice line** in the lesson with `--->` and shows its
instruction in the HUD. There are two kinds:

- **Edit exercises** — change the practice line to satisfy a condition (delete a
  word, replace some text, indent it). The tutor watches your edits and detects
  success automatically: the HUD flashes **`STAGE CLEAR!`** the moment the line
  is right. Press `<CR>` to bank it and move on.
- **Observational exercises** — for things that aren't a single-line edit (run a
  command, open a split, jump to a definition). These just say "do X, then press
  `<CR>`": there's nothing to auto-detect, so `<CR>` always advances.

`<C-k>` steps back to the previous exercise if you want to practice it again.

**Hints** appear in the HUD automatically after a couple of unsuccessful edit
attempts on the same exercise — so you're never stuck, but you get a beat to
work it out first.

## The HUD (lives, score, high score)

The pinned top row is the scoreboard:

```
 ♥♥♥ | LV.2-3 | SCORE:   140 | HI:   320 | Delete the word under the cursor
```

- **`♥♥♥`** — your **lives** (3 to start). Pressing `<CR>` while an edit
  exercise's condition is *not* met costs one life. (Observational / already-met
  exercises never cost a life.)
- **`LV.<lesson>-<exercise>`** — where you are.
- **`SCORE`** — points banked this run; **`HI`** — your best score for this
  lesson, **persisted across restarts**.

When you clear the last exercise the HUD shows **`LESSON CLEAR!`** (with
`NEW RECORD!` if you beat your high score) — press `<CR>` to roll into the next
lesson. Finish all seven and you get **`YOU WIN!`**.

Run out of lives and it's **`GAME OVER`** for that exercise: `<C-k>` reloads the
lesson to try again, or `<CR>` skips the exercise (no points) and moves on.

## Notes

- The lesson buffer is a real buffer — `:w` it elsewhere, split it, `:bn` away
  and back; the session and HUD ride along with it.
- High scores live in your config dir and are per-lesson, so chasing a better
  `HI` is a reason to replay.

## See also

- [Modal editing](help:modal-editing) — the grammar lessons 1–2 drill.
- [Modes](help:modes) and [Help](help:help) — what lesson 4 covers.
- [Themes](help:themes) / [Options](help:options) — what lesson 7 covers.
