---
summary: "preview-mode: marks a pane showing a picker preview — an isolated, read-only projection that never touches the buffer you came from."
related: [picker, preview]
---

# preview-mode

Marks a pane that is showing a **picker preview** — the file under the
cursor in `:picker files`, rendered in place so you can see what you're
about to open.

The point of the mode is isolation. A preview is a read-only
*projection*: it never mutates the buffer the pane is committed to,
never touches the editor's active-buffer state, and never disturbs that
buffer's options or mode stack. Dismiss the picker and the pane snaps
back with nothing to undo, because nothing was swapped out.

That matters because previewing is incidental — you're scrolling a
list, not deciding to open anything yet. A preview that left traces
would make browsing a picker destructive.

## Behaviour worth knowing

- **Read-only**, and the buffer is ephemeral — it is garbage-collected
  when the preview ends rather than accumulating in `:ls`.
- **Never runs the expensive open path.** A preview does not attach a
  language server or run the full parse; that work waits until you
  actually open the file.

## Options

`ReadOnly = true`, plus the ephemeral buffer flag so the preview is garbage-collected rather than kept.

## Keybindings

None. The keys you press while previewing belong to the picker — see [`picker`](help:picker).

## See also

- [`picker`](help:picker) — the pickers that preview.
