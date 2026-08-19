---
summary: "plugin-trace-mode: major mode for the *plugin-trace* buffer — the host/guest boundary trace, drained off-thread."
related: [plugins, plugin-manager]
---

# plugin-trace-mode

Major mode for the `*plugin-trace*` buffer: every call across the
host/plugin boundary, in order, with timings.

## What it is for

Answering "what is this plugin actually doing, and what is it costing?"
The trace records the boundary — calls in, calls out, how long each took
— which is the level at which a slow or chatty plugin is diagnosable
without a debugger.

## Why the buffer, and not a log file

It is an ordinary buffer, so searching it is `/`, filtering it is the
same motions as anywhere else, and it can sit in a split next to the
thing it is describing. Records stream in off the UI thread; the trace
never runs on the path a keystroke takes.

## Verbosity

`plugin.trace-level` controls how much is recorded, live:

```
:set plugin.trace-level=debug
```

Raising it costs boundary overhead, which is why it is not on by
default.

## Keybindings

The buffer is read-only and otherwise ordinary — every motion and search
chord works.

## See also

- [`plugins`](help:plugins) — loading, reloading, and the manager view.
