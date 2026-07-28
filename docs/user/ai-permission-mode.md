---
summary: "ai-permission-mode: the popup answering an agent's permission request — CR selects the option under the cursor, Esc or q defers it."
related: [ai, agent, acp, permission]
---

# ai-permission-mode

The popup an agent's **permission request** opens: the agent wants to
do something (write a file, run a command), and this is where you say
yes, no, or which variant.

## Chords

| Chord | Action |
|---|---|
| `<CR>` | Select the option under the cursor |
| `<Esc>` or `q` | Defer — leave the request pending |

## Deferring is not denying

`<Esc>` and `q` dismiss the popup **without answering**. The request
stays pending and the agent stays blocked on it; you can come back and
answer later.

That's deliberate: a permission prompt appearing while you're
mid-thought shouldn't force a decision, and dismissing a dialog is a
reflex. Making dismissal mean "not now" rather than "no" keeps the
reflex from silently denying something you meant to allow.

To answer, pick an option and press `<CR>`.

## The option list is the agent's

The options come from the request itself, not from a fixed yes/no set —
an agent may offer "allow once", "allow for this session", "deny", or
something specific to what it's asking. The popup renders whatever the
agent sent, so read them rather than pressing by position.

## Avoiding the prompt entirely

`<C-t>` in the [conversation buffer](help:ai-conversation-mode) toggles
trust mode. In auto-accept, requests are granted without surfacing
here. Worth it for a long mechanical task you're supervising; not worth
it otherwise.

## See also

- [`ai-conversation-mode`](help:ai-conversation-mode) — the
  conversation and the trust toggle.
