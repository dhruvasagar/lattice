# Mouse

Lattice reads the mouse in both the terminal and the GPUI window.

| Gesture | What it does |
|---|---|
| Wheel / trackpad scroll | Scrolls the pane **under the pointer**, three lines a notch |
| Left click | Moves the cursor there, and focuses that pane |
| Left drag | Selects, in Visual mode |
| Click a modeline element | Runs whatever that element declares |

## Scrolling doesn't steal focus

The wheel scrolls whichever pane the pointer is over, and leaves the
focused pane alone. So you can keep typing in one split while spinning
through a reference file in another — no click first, and no getting
bounced out of what you were doing. Vim, Helix and Zed all behave this
way.

Clicking is the gesture that *does* move focus. Click into a split and
you're in it.

## Dragging gives you a Visual selection

A drag doesn't produce some separate "mouse selection" that only a
`Ctrl-C` understands. It leaves you in **Visual mode**, with the region
live, exactly as if you had pressed `v` and moved. So everything works
on it:

- `d` deletes it, `y` yanks it, `c` changes it
- `>` indents it, `gu` lowercases it, `:` operates on its range
- `o` jumps to the other end, `iw` / `ap` expand it

Drag from where you want the selection to start; the anchor stays put
while you move. A plain click ends the selection, as it does anywhere
else.

## Turning it off

`ui.mouse` is on by default, and it has one real cost: while Lattice is
reading the mouse, your **terminal emulator** isn't. Its own click-drag
text selection and middle-click paste stop working inside Lattice. Many
terminals let you hold **Shift** to get them back — try that first, it
is usually all you need.

If your terminal has no such override, or you'd rather it kept the
mouse:

```
:set ui.mouse=false
```

That applies immediately, so flipping it off to copy something and back
on afterwards is perfectly reasonable. To make it stick, put this in
your config:

```toml
[ui]
mouse = false
```

The GPUI window ignores the option — it owns its own input, so listening
for the mouse costs you nothing there.

## What doesn't respond yet

- **Full-screen programs inside `:terminal`.** A terminal buffer gets
  ordinary editor gestures — scroll its scrollback, click to position,
  drag to select. Passing the mouse through to the program running
  inside it (so `htop` or a nested editor sees your clicks) is a
  separate piece of work and isn't built.
- **Clicking in the gutter**, on a fold marker, or on a sticky-context
  header. These are inert for now. Scrolling over them works, because
  the pane underneath is still the pane.
- **Click and drag in the GPUI window.** Scrolling works there; cursor
  positioning is terminal-only so far.
