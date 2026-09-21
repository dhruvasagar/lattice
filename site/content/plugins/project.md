+++
title = "project"
description = "Pick the project first, then the verb — find a file, grep, open a shell or Magit in a project you are not currently in."
weight = 12
[extra]
kind = "bundled"
+++

Every other project-aware surface in lattice roots itself at the buffer you
are standing in. This one is for the project you are **not** in.

`<leader>pp` — or `<C-x>pp` if you use the emacs bindings — picks a project,
then offers the verb: find a file in it, grep it, open a shell there, or open
Magit on it. Choosing the project first is the whole idea; it is the order
that matters when you are switching context rather than working.

## Getting it

Ships with lattice, on by default. To turn it off:

```
:set project.enabled=false
```

## Documentation

`:help project`, or [core plugins](@/docs/config/core-plugins.md).
