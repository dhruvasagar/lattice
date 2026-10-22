# Insert-completion integrity — slice plan

> **Slice plan.** Sequencing, slice IDs, status.
> Design contract: [`../../architecture/insert-completion.md`](../../architecture/insert-completion.md)
> — §3.7 (aggregator), §4 (triggering), §3.3 (source shape).

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

**Status: 🚧 in progress (2026-10-22).**

## Why

Every source is contributed by a mode and both renderers share one host
pipeline, so the *sources* are sound. What loses their candidates is the
merge, and nothing caught it because no test presses a real `<C-Space>`:
every source's tests call `do_completion_trigger()` directly. An audit
(2026-10-22) confirmed, in code:

1. **A finished round drops the rest.** The drain clears the pending-round
   token on the FIRST outcome and closes the popup when that outcome leaves
   it empty; later outcomes hit the "no popup" return and are discarded.
   Org's completion carrier is `Universal` and answers empty at once outside
   `.org`, so in a Rust buffer it can close the popup before LSP answers.
   The after-edit refresh closes on empty while a round is in flight, too.
2. **Ranking forgets priorities.** The bonus at open
   (`priority_for_source`) knows five sources; the copies in the drain and
   the after-edit refresh know three. Tree-sitter and path rows fall to 0
   after the first keystroke or the first async landing.
3. **Plugin sources rank last.** `CompletionSourceContribution::default_priority`
   is never read; any id outside the builtin five scores 0.
4. **The source filter leaks.** The drain re-matches without
   `state.source_filter`, so after `<C-b>` an async batch's rows return.
5. **`.txt` misses its defaults.** The per-language default is keyed
   `text`; `active_language_id` returns `txt`.
6. **Late plugin carriers miss open buffers.** Policy minors activate on
   `MajorEntered` only; a carrier registered after a buffer's major was
   entered never reaches it.
7. **Unstamped plugin rows accumulate.** The drain replaces rows by
   `source`; the host takes a plugin candidate's `source` from the guest,
   and the WIT does not require it.
8. **Auto-trigger is specified (§4.2) and unbuilt.** `completion.auto_trigger`
   does not exist; per-language `auto_trigger`, `trigger_chars`,
   `IdentifierThreshold` / `TriggerChar` and LSP `triggerCharacters` are
   read by nothing. Decided 2026-10-22: **manual stays the default**;
   auto-trigger is opt-in, as §4.2 says.

## Slices

| Slice | What | Gate | Status |
|---|---|---|---|
| IC.1 | End-to-end: a real `<C-Space>` through each renderer's dispatch, per source — buffer words, tree-sitter, path, snippet, LSP (in-process mock server via `attach_handle`), an async non-LSP source | each source's candidate is in the popup the key opened | ✅ |
| IC.2 | A round is not over until every source has answered: the popup stays while any is pending, "no completions" only when all answered empty, the after-edit refresh does not close mid-round (finding 1) | test: an empty async source landing before LSP fails without the fix | ✅ |
| IC.3 | One match / rank / dedup path for open, refresh and drain; the bonus reads every source's priority (contribution default, option override); the drain honours the filter (findings 2, 3, 4) | tests per finding, each failing without the fix; bench: refilter cost per keystroke | ✅ |
| IC.4 | Plugin source identity: the host stamps the spec id on plugin rows; popup label and filter chord come from the contribution; policy minors registered late activate on open buffers (findings 6, 7) | tests: unstamped rows replaced not accumulated; a carrier registered after open reaches the buffer | ✅ — filter chords stay the five builtins: a plugin's contribution declares no chord (the WIT has no field for one), so deriving them from contributions would change nothing; the footer label does come from the source |
| IC.5 | `.txt` maps to `text`; stale comments (`options.rs`, TUI `App::apply`, `completion.rs` "stub", `source.rs` module doc) (finding 5) | test: a `.txt` buffer gets the `text` source set | 📝 |
| IC.6 | Auto-trigger, opt-in (§4.2): `completion.auto_trigger` (default `false`) + per-language override, trigger chars (sources' and LSP's `triggerCharacters`), identifier threshold `completion.min_chars` (default 2); `rust`'s per-language default stops forcing it on (finding 8) | tests: off by default; on → trigger char and threshold open it; the typed char lands first; bench: per-keystroke cost with auto-trigger on | 📝 |
| IC.7 | `:describe-buffer` lists the buffer's completion sources: each one's id, priority, whether its language enables it, and the condition it answers under (LSP: a server attached; paths: inside a string) | test: the section names every active source, and a disabled one as disabled | 📝 |
| IC.8 | Rank indices into `raw` instead of cloning each candidate twice (into `ScoredCandidate`, then `RenderedCandidate`) — the bulk of the re-rank's cost per keystroke (benchmarks.md, IC.3: 554 µs at 2000 rows); `ScoredCandidate` is shared with the picker | bench: the IC.3 rows drop; picker benches unchanged or better | 📝 |
