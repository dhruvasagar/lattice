# `comment` — a toggle-comment operator as a core plugin

**Goal:** `gc{motion}` / `gcc` / `gc` in Visual, contributed from WASM, shipped
as the fourth core plugin.

**Why it is worth doing beyond the feature.** All four reference editors have
it — Vim (commentary), Neovim (built in since 0.10), Helix (`C-c`), Zed
(`Cmd-/`), Emacs (`M-;`) — and lattice has no comment operator at all. But it
is chosen as the first new plugin because of what it proves: paramount goal #3
says "adding new motions / text objects / operators is first-class", and
nothing demonstrates that like an **operator contributed across the WASM
boundary** composing with every motion and text object without a line in
`lattice-grammar`.

It is deliberately NOT blocked on `plugin-inline-decorations.md` — that seam
unblocks colorizer and todo-comments; `gc` needs only the grammar seam.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred · ❌ dropped.

| Slice | What | Gate | Status |
|-------|------|------|--------|
| C.1 | `apply-operator` gets `doc`; `document` gets `comment-syntax` | a fixture guest operator reads a line and its comment leader | 📝 |
| C.2a | A plugin operator can declare its chord | `gc{motion}` / `gcc` / Visual `gc` resolve for a fixture operator | 📝 |
| C.2b | The `comment` plugin itself | `gcc` round-trips in rust / python / lua buffers | 📝 |
| C.3 | Promote to core | a release archive carries four plugins; `:plugins` shows four bundled | 📝 |

---

## Task C.1: let an operator read the buffer it operates on

**The gap.** `apply-operator` is the ONLY grammar callback that receives
neither `doc` nor `tree`:

| callback | `doc` | `tree` |
|---|---|---|
| `apply-motion` | ✅ | ✅ |
| `apply-text-object` | ✅ (OM.4b) | ✅ (OT.1) |
| `apply-action` | ✅ (AP.0.1) | ✅ (TS.1) |
| `apply-ex-command` | ✅ (OC.10) | ✅ (OC.10) |
| **`apply-operator`** | ❌ | ❌ |

This is incremental wiring that never reached operators, not a decision — no
plugin had contributed one. A comment operator cannot work without the text:
deciding comment-vs-uncomment, finding the indent column, and stripping an
existing leader all mean reading lines.

**Scope: `doc` only, not `tree`.** `OperatorContext` (native) carries
`document` and `comment_syntax` but — unlike `TextObjectContext` — has no
`path` or `syntax` field, so minting a tree resource would mean widening the
native context and touching every operator call site. A comment operator does
not need the tree: with the leader and the text it has everything. Tree parity
for operators is left until an operator wants it, with the gap now named here
rather than silent.

**The comment syntax rides the `document` handle, not the context.** That is
what `types.wit` already says — `text-object-context`'s doc comment reads
"buffer text + the scope/comment env ride the `document` handle" — and
`buffer.wit`'s `document` resource never grew the accessor. Putting it there
rather than on `operator-context` means one accessor serves all four callbacks
that hold a `doc`, instead of the same field copied into each context record.

- [ ] **Step 1: `buffer.wit` — add the accessor**

	record comment-syntax {
	    /// Line-comment leader, e.g. `//` or `#`. `none` for a language
	    /// with no line comment form.
	    line: option<string>,
	    /// Block delimiters, e.g. `("/*", "*/")`.
	    block: option<tuple<string, string>>,
	}

	// on `resource document`:
	/// The buffer's comment syntax, as the host resolved it for this
	/// buffer's language. `none` for plain text or an unknown language —
	/// a caller degrades to "cannot comment here", never guesses.
	comment-syntax: func() -> option<comment-syntax>;

Mirrors `lattice_grammar::registry::CommentSyntax` field for field.

- [ ] **Step 2: `grammar.wit` — give `apply-operator` the `doc` borrow**

	apply-operator: func(
	    callback: u32,
	    ctx: operator-context,
	    doc: borrow<document>,
	) -> result<list<effect>, string>;

Note in the WIT why there is no `tree`, so the asymmetry reads as a decision.

- [ ] **Step 3: host — mint the doc in `build_operator_spec`**

`grammar_trampoline.rs:357`. Mirror `build_text_object_spec` exactly: build a
`DocumentSnapshot`, push a `DocumentResource`, borrow, call, delete. The
operator context has `document: &mut Document` rather than `buffer`, so the
snapshot's buffer comes from there; `path` is absent on the native context, so
it is `None` until the native context grows one.

- [ ] **Step 4: host — implement `comment-syntax` on `DocumentResource`**

The snapshot must carry the resolved `CommentSyntax`. `DocumentSnapshot` gains
a field the trampolines populate from whichever context has it
(`OperatorContext::comment_syntax`, `TextObjectContext::comment_syntax`);
`None` elsewhere, which is the honest answer rather than a guess.

- [ ] **Step 5: a fixture guest that proves it**

Extend the grammar fixture with an operator that reads line 0 through `doc` and
its leader through `comment-syntax`, and returns an edit derived from both.
Assert host-side that the edit reflects the real text — **not** that the call
succeeded. Per `none-paths-are-not-coverage`, a seam tested only through its
`None` answers passes whether or not it works, so the test must run on a buffer
whose language HAS a leader.

- [ ] **Step 6: gates + commit**

`scripts/precommit.sh lattice-plugin-host lattice-grammar`. The WIT is the
public plugin API — `wit-ownership.md` governs; check whether the change needs
a version note there.

---

## Task C.2a: a plugin operator can declare its chord

**The gap, and it is the third of the same shape.** `register-operator` lets a
plugin *register* an operator. Nothing lets that operator be *reached*:

- `keymap.wit`'s `binding-mode` is normal / insert / visual / select / replace
  / command / search. It says outright that "the transient operator-pending /
  after-key states are internal grammar states, **not plugin-bindable**".
- So a plugin cannot bind `gc` into `AfterG`, cannot get operator-pending
  motion targets, the doubled `gcc`, `i_`/`a_` text-object pendings, or the
  `f`/`F`/`t`/`T` find-char pendings.
- Binding `gc` in plain Normal through `register-binding` does not work
  around it: it fires immediately with no motion, and a bound prefix kills its
  longer chords, so it would also kill `gcc`.

The host builds all of that for native operators in
`keymap_normal::register_operator_bindings`, which is **already `pub` for this
exact case** — N.1.3 exposed it so boot could wire a *provider-contributed*
operator's chord (narrow's `zn`) while the provider kept the spec and apply.
A WASM plugin is the same split with a process boundary in the middle.

**Chosen (Dhruva, 2026-09-22): the operator declares its chord.**
`operator-spec` gains `chord` and `doubled`; the loader's grammar drain wires
them. Rejected: a separate `register-operator-chord` on the keymap seam —
it splits one concept across two seams and permits an operator that is
registered but never reachable.

**Layer: `MinorMode(<mode>)`, not `Builtin`** — and
`register_operator_bindings` gains a `layer` parameter to allow it. The
standing rule says feature keymaps never sit at Builtin, and there is a
concrete reason here beyond the rule: at Builtin the chord outlives its plugin,
so `:set comment.enabled=false` would leave `gc` bound to a command whose
handler is gone. At MinorMode it is scoped and reverses with the plugin, which
is what the loader already does for mode-plugin bindings.

**Wiring shape — a service, like every other seam.** `register_operator_bindings`
lives in `lattice-host` and the drain lives in `lattice-plugin-loader`, which
does not depend on it. So the host publishes a handle:

	// lattice-mode
	pub trait OperatorChordWirer: Send + Sync {
	    fn wire(&self, op: OperatorId, chord: &str, doubled: Option<char>,
	            mode: ModeId, post_motion_char: bool);
	}
	pub type OperatorChordWirerHandle = Arc<dyn OperatorChordWirer>;

`lattice-host` implements it over `register_operator_bindings`;
`LoaderServices` carries it; **absent ⇒ the seam is `NotWired` and the load
fails loudly**, per the `agenda_registry` precedent — a plugin whose operator
silently has no keys is the failure this project keeps writing rules about.

- [ ] **Step 1: `wit/types.wit`** — `operator-spec` gains `chord: option<string>`
  and `doubled: option<string>`. Note in the WIT that `doubled` is the TRAILING
  key (`c` for `gcc`, `U` for `gUU`), not the whole chord, and that `none`
  binds no doubled form (vim has no `zff`).
- [ ] **Step 2: `lattice-mode`** — the trait + handle.
- [ ] **Step 3: `register_operator_bindings`** — take `layer: KeymapLayer`.
  Native callers pass `Builtin` unchanged.
- [ ] **Step 4: `lattice-host`** — implement the trait, register the service.
- [ ] **Step 5: `lattice-plugin-host`** — carry `chord` / `doubled` out of the
  WIT spec to the drain. The native `OperatorSpec` has no chord field and
  should not grow one; the chord is registration data, not dispatch data.
- [ ] **Step 6: `lattice-plugin-loader`** — `LoaderServices.operator_chords`;
  wire on drain; `NotWired` when absent.
- [ ] **Step 7: test** — the fixture's `comment-probe` gains a chord, and a
  host test presses `gc` + a motion, then `gcc`, and asserts the operator ran
  over the right range. Pressing keys, not calling `dispatch_chord` — per
  `dispatch-chord-cannot-compose-operators`, a host-side harness firing
  `d<motion>` runs the bare motion.

## Task C.2b: the plugin

- [ ] **Step 1: `plugins/comment/`** — a standalone `wasm32-wasip2` crate, not
  a workspace member (the `plugins/*` precedent). `plugin.toml`:
  `provides = ["grammar", "keymap", "config", "help"]`.

- [ ] **Step 2: the operator.** `register-operator("comment-toggle", …)`.
  Toggle semantics follow the field, which agrees: if **every** non-blank line
  in the range is already commented, uncomment; otherwise comment all of them.
  Comment at the **minimum indent column** of the range, not at column 0 —
  that is what Vim's commentary, Neovim's built-in and Zed all do, and
  column-0 insertion in indented code is the thing users notice first.
  Blank lines are skipped, not commented.

- [ ] **Step 3: the leader.** From `doc.comment-syntax()`. When it is `none`,
  the operator echoes "no comment syntax for this buffer" and returns no edit
  — the `graceful error handling` half of the four-artefact rule; never guess
  `//`.

- [ ] **Step 4: chords.** `gc` (operator, awaits a motion), `gcc` (linewise,
  current line), `gc` in Visual. Registered at
  `KeymapLayer::MinorMode(comment-mode)` per the mode-ownership rule, never
  `Builtin`. **`gc` and `gcc` cannot both be plain bindings on the same trie**
  — see `a-bound-prefix-kills-its-longer-chords`; `gcc` is the doubled-operator
  form the grammar already handles for `dd` / `yy`, so it must go through that
  path rather than a second binding.

- [ ] **Step 5: `doc/comment.md`.** Required, not optional: it is the page
  `:help comment` serves AND the body `site/content/plugins/comment.md` is
  generated from. `sync-docs.sh` fails without it once C.3 lands.

- [ ] **Step 6: tests.** Round-trip (comment then uncomment restores the
  buffer byte-for-byte) in a `//` language, a `#` language, and a buffer with
  no leader. Mixed-indent range comments at the minimum column. A range that
  is already fully commented uncomments.

---

## Task C.3: promote to core

Adding a core plugin is a six-file change, and three of them fail loudly if
missed (which is the point).

- [ ] **Step 1: `xtask/src/main.rs`** — `CORE_PLUGINS`.
- [ ] **Step 2: `.github/workflows/release.yml`** — it hardcodes
  `for pl in auto-pair treesitter-context project` in **four** places
  (lines ~94, ~231, ~247, ~330). All four verify staged artefact contents; a
  missed one ships an archive without the plugin, silently, which is the exact
  defect L.2 existed to fix.
- [ ] **Step 3: `site/data/plugins.toml`** — else `sync-docs.sh` hard-fails
  (`CORE_PLUGINS ships plugins absent from site/data/plugins.toml`).
- [ ] **Step 4: `docs/user/core-plugins.md`** — the table row.
- [ ] **Step 5: verify** — `cargo xtask build-core-plugins`, then `:plugins`
  shows four bundled rows, `:help comment` opens the manual, and
  `python3 site/scripts/sync-docs.sh && zola build` are clean.
