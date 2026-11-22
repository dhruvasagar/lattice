# Slice plan — org babel

Design: [`org-babel.md`](../../architecture/org-babel.md). Read it first; this
plan argues from it and does not repeat it.

Status icons: ✅ done · 🚧 in progress · 📝 planned · ⛔ deferred (not yet) · ❌ dropped (not at all).

Two repositories. **(lattice)** slices land in this tree and ship in a lattice
release before any **(plugin)** slice may use them. **(plugin)** slices land in
[`lattice-org-plugin`](https://github.com/dhruvasagar/lattice-org-plugin), all
of them inside `org-babel-mode` (design §0).

| Slice                     | Description                                                                | Status |
|---------------------------|----------------------------------------------------------------------------|--------|
| **Phase 1 — execute**     |                                                                            |        |
| OB.1                      | Option-bound grants may read a list **(lattice)**                          | ✅     |
| OB.2                      | `proc:spawn:{option:field}` — the option-bound process grant **(lattice)** | ✅     |
| OB.3                      | `post-effects` — effects from the events seam **(lattice)**                | ✅     |
| OB.R1                     | lattice release carrying OB.1–OB.3 **(lattice)**                           | 📝     |
| OB.4                      | `org-babel-mode`: the mode, its layer, its options **(plugin)**            | 📝     |
| OB.5                      | Src blocks and header arguments, parsed and merged **(plugin)**            | 📝     |
| OB.6                      | `C-c C-c` runs a block; output lands in `#+RESULTS:` **(plugin)**          | 📝     |
| OB.7                      | Confirm, cancel, execute-buffer / -subtree, remove-result **(plugin)**     | 📝     |
| OB.8                      | `:results` — insertion, format, `value` wrappers, `:dir` **(plugin)**      | 📝     |
| **Phase 2 — tangle**      |                                                                            |        |
| OB.9                      | `write-to-file` gains a `whole` anchor **(lattice)**                       | 📝     |
| OB.10                     | `:tangle` **(plugin)**                                                     | 📝     |
| **Phase 3 — cross-block** |                                                                            |        |
| OB.11                     | `:var` — literals, named results, named tables **(plugin)**                | 📝     |
| OB.12                     | Dependency chains — a missing result is run first **(plugin)**             | 📝     |
| OB.13                     | noweb **(plugin)**                                                         | 📝     |
| **Phase 4 — sessions**    |                                                                            |        |
| OB.14                     | `spawn-interactive` + job stdin **(lattice)**                              | 📝     |
| OB.15                     | `:session` **(plugin)**                                                    | 📝     |

## Where this stands (2026-11-22)

**Phase 1's lattice half is done: OB.1–OB.3 ✅, on branch `org-babel-phase-1`**
(not yet merged to `main`, not pushed when this was written):

| Commit | Slice |
|---|---|
| `7601db92` | design fragment + this plan |
| `00fd3690` | OB.1 — list-valued option-bound grants |
| `f21e2d5a` | OB.2 — `proc:spawn:{option:field}` |
| `1cd970e5` | OB.3 — `post-effects` |

**Verified per slice:** the slice's own tests, `cargo clippy --lib --bins`
(zero production warnings), rustdoc + doc coverage, and the site build — the
commands and counts are in each slice's *As built* block below.
**Not yet run, and owed before merging:**

1. The whole-crate suites for the crates touched — `lattice-config`,
   `lattice-plugin-host`, `lattice-plugin-loader`, `lattice-wit`,
   `lattice-plugin-api` — and `lattice-host` (only its boot pin was run). One
   crate at a time, throttled (`CARGO_BUILD_JOBS=3 nice`), when the machine is
   free.
2. A fresh whole-branch code review of `7601db92..1cd970e5`, with the Review
   focus list above and the decision log below as its brief.

**Next:** OB.R1 (the release — see its section for what it must bump), then
the plugin slices from OB.4 in `lattice-org-plugin`.

**Toolchain note:** `scripts/docs-check.sh` runs rustdoc on `stable`; on the
machine this was built on that was rustc 1.93, which some dependencies refuse.
Run it with `DOCS_CHECK_TOOLCHAIN=1.94.0` (the pinned toolchain) until `stable`
is updated — the failure is environmental, not the branch's.

## Decision log

Every call made while building, beyond what the design states, with what it
costs if it turns out wrong. Design-level decisions are also in
[`org-babel.md`](../../architecture/org-babel.md); this is the complete list.

| Slice | Decision | Why | Cost if wrong |
|---|---|---|---|
| OB.1 | Each entry of a list grant is its own `OptionBoundGrant` (same `option`), not a `Vec` on one | No consumer type changes; `:plugins`, refusals and the loader read it as before | `:plugins` shows one line per directory |
| OB.1 | `announce_moved_grants` compares per option over all entries (`moved_grant_options`) | Pairing entries one-for-one missed a list that GAINED an entry, so the plugin never reloaded onto it | none |
| OB.1 | The grant reads the option as a `ConfigValue`, and a held value whole (`ConfigRegistry::held_config_value`) | A list flattened to text loses its entries | none |
| OB.2 | Syntax `proc:spawn:{option:field}` / `{option}`, not `{option.field}` | Option names contain dots | a manifest string to change before org ships |
| OB.2 | No `\|default` on a process grant | Nothing runs until the user configures it | none |
| OB.2 | Commands compared as written, no `PATH` resolution | `sh` must not silently permit `/bin/sh` or another `sh` | a user writing `/usr/bin/python3` in config must request exactly that |
| OB.2 | **A process grant follows only user-authored values** (`Authorship::UserOnly`): an option still at the default the plugin registered grants nothing | Security review: a plugin defaulting its list to `sh` would grant itself a shell | a user who sets the option to exactly the plugin's default is ignored (no provenance on option values) |
| OB.2 | **Remembered values carry whether the user wrote them** (`GrantGuard::remembered: (ConfigValue, bool)`) | Security review: a directory grant on the same option remembered the default and laundered it into the process grant after a reload | none |
| OB.2 | `register-server` still needs unbound `proc:spawn` | An option-bound grant was not given to register language servers | none |
| OB.2 | `:plugins` shows the template (`proc:spawn:{org.babel.languages:command}`), not the resolved commands | Smallest change; the option is what the user edits | the user sees the option, not the list |
| OB.2 | The grammar-seam spawn is covered by the shared `process_host::prepare` check, not a grammar fixture; cancel-on-reload by the existing `PluginState`-owned job teardown | Both seams reach the same check; teardown is already tested | a seam-specific regression shows only in `prepare`'s tests |
| OB.3 | A refused posted effect becomes the usual warning and the call answers `ok`; `err` only when no sink is wired | Same as a returned effect; the plan's "err names the first refused" would have made posting differ from returning | a guest cannot branch on a refusal — the user sees it instead |
| OB.3 | The sink is wired in `lattice-plugin-loader::install` from `boot.inbound`; no `LoaderServices` field; `lattice-host` untouched | The loader already owns plugin seam wiring; `WiredSeams.effect_sink` is the boot pin | none |
| OB.3 | "Without a keypress" proven by composition, not a new full-editor plugin test | Each link already had a test: the post (`post_effects_seam.rs`), the inbound wake (`lattice-mode`), a drained `ApplyEdit` (`off_keystroke_applies_edits.rs`), the wire (boot pin) | a break where the links meet is caught only by the boot pin |
| OB.3 | No WIT package version bump | Additive; the ABI fingerprint already marks built plugins stale | OB.R1 must bump the published crates before org can use it |
| OB.3 | "Both linkers" is structural — one bindgen `host-services` interface, one `PluginState` impl | Adding a function to the interface adds it to both | none |

## Global constraints

- **Mode ownership (design §0).** Every plugin slice's code is under
  `src/babel/` in the plugin, registered by `org-babel-mode`. No babel chord in
  org's major layer; no branch in `ctrl_c_ctrl_c`. A lattice slice whose diff
  contains "org" or "babel" outside docs and test fixtures is wrong.
- **Lattice acid test:** zero `Editor::` methods, zero host `Action` variants,
  no `BufferKind`, no `Lang` arm.
- **Byte-compatibility with org-babel** (design §1): `#+RESULTS:` shapes,
  header-argument grammar and property inheritance as emacs writes them.
- **Default-off execution:** `org.babel.languages` defaults to empty;
  `org.babel.confirm` defaults to `true`; `org.babel.enabled` defaults to
  `true`; `org.babel.min-lines-for-block-output` defaults to `10`;
  `org.babel.max-output-lines` defaults to `10000`.
- **Ex-commands** are `org-babel-execute`, `org-babel-execute-buffer`,
  `org-babel-execute-subtree`, `org-babel-tangle`, `org-babel-remove-result`,
  `org-babel-cancel`. Chords: `<C-c><C-c>`, `<C-c><C-v>b`, `<C-c><C-v>s`,
  `<C-c><C-v>t`, `<C-c><C-v>k`.
- **Async results reach the screen without a keypress.** Every test of a
  result asserts it is visible *without dispatching another action*.
- **Production code: zero warnings**, `unwrap`-free, per the precommit gate.
  Each lattice slice runs `scripts/precommit.sh lattice-plugin-host` (plus
  `lattice-wit` where WIT changes); each plugin slice runs the plugin's own CI
  script. Whole-crate gates once per batch, not per slice.
- **Four artefacts per non-trivial slice**: the design (already written —
  update it only if the design changes), tests including failure modes, a
  bench where a hot path is added, graceful errors (log + skip, never panic).
- **Docs land on the site.** User-facing changes update the plugin's
  `doc/org.md` and `:help org-babel`, and lattice's `docs/user/org.md`.

## Review focus

Input classes the design implies but no happy-path test would hit. Each has a
test in the slice that owns it.

1. **Output that looks like org.** A line of program output beginning with
   `*` or `#+` would become a headline or a keyword. Verbatim results prefix
   `: `; inside `#+begin_example` such lines are comma-escaped (`,*`, `,#+`)
   exactly as org does (OB.6, OB.8).
2. **The block changed or vanished while running.** Body edited, block
   deleted, buffer closed, or two identical unnamed blocks — the result must
   go to `*org-babel*` or to the right block, never to a stale offset (OB.6).
3. **The configured command and the spawned string differ.** `python3` in the
   table and `/usr/bin/python3` requested: refused, with the option named.
   Matching is the exact string as configured (OB.2).
4. **A program that never exits, or floods.** No timeout exists; cancel must
   kill the tree and still produce exactly one finish. Output past the cap is
   truncated with a count, not buffered unbounded (OB.6, OB.7).
5. **The user edits `org.babel.languages` mid-run.** Grant change ⇒ reload ⇒
   jobs cancelled; the block keeps its old result and the echo says why
   (OB.2, OB.6).

---

## Phase 1 — execute

### OB.1 — option-bound grants may read a list ✅ *(lattice)*

`fs:write:{opt}` binds one path. Let the option be a `list<string>`, each
entry one `FsGrant`; the scalar form is the one-element case.

**Files:** `crates/lattice-plugin-host/src/capability.rs` (`grant_with`, ~l.146),
`crates/lattice-plugin-host/src/manifest.rs` (`OptionPath`, ~l.110),
`crates/lattice-plugin-host/src/lib.rs` (`grant_for` ~l.4933; PMF.4/5 tracking
~l.1020–1120).

**Interface change:** the `option_value` closure handed to `grant_with` returns
the option's *values*, `Option<Vec<String>>`, not one string — a list option
yields its entries, a scalar yields one. `OptionBoundGrant` records the
values it resolved, so PMF.4's "grant changed ⇒ reload" compares lists.

**Tests** (`capability.rs` unit tests, plus one in
`crates/lattice-plugin-host/tests/` that reloads):
- `a_list_option_grants_each_entry` — `["~/a", "/b"]` ⇒ two `FsGrant`s, tilde
  expanded.
- `an_empty_list_grants_nothing` — and no fallback to the default.
- `a_scalar_option_still_grants_one` — the existing tests stay green unchanged.
- `changing_a_list_entry_reloads_the_plugin` — PMF.4 over a list.
- `a_plugin_cannot_set_its_own_list_grant_option` — PMF.5 refuses `set-option`.


**As built** (`00fd3690`): `grant_with`'s closure returns
`Option<ConfigValue>` and resolves through `option_paths`; a registered option
is read with `get_value()`, an unregistered one from
`ConfigRegistry::held_config_value` (new) or the remembered value. Tests:
`a_list_option_grants_each_entry`, `an_empty_list_grants_nothing_and_does_not_fall_back`,
`blank_and_non_string_list_entries_grant_nothing`, `a_record_value_grants_no_path`
(`capability.rs`); `a_held_list_is_in_the_first_grant_entry_by_entry`,
`a_registered_list_option_grants_its_entries_and_tracks_changes`,
`a_plugin_cannot_set_its_own_list_grant_option` (`lib.rs`);
`held_config_value_keeps_a_list_whole_and_a_scalar_as_a_string`
(`lattice-config`); `moved_grant_tests` ×3 (`lattice-plugin-loader`). The
reload test the plan named is the loader's existing `grant_follows_option.rs`,
which now compares per option.

### OB.2 — `proc:spawn:{option:field}` ✅ *(lattice)*

**Manifest form:** `proc:spawn:{org.babel.languages:command}` — an option name,
a colon, and a field name, resolving to the `command` field of each record in a
list-of-records option. A colon, not a dot, because option names already
contain dots (`org.babel.languages.command` could be an option of its own).
`proc:spawn:{opt}` with no field reads a list of strings. Parsed into a new `Capability::ProcSpawnOption {
option, field }` in `manifest.rs`; the option must be in the plugin's own
namespace (the same check as `FsWriteOption`).

**Grant:** `Grant` gains `spawn_commands: BTreeSet<String>`. Both tiers may
receive it. Unbound `proc:spawn` stays bundled-only and still sets
`proc_spawn = true`, which permits any command.

**Check:** in `spawn_process` (`lib.rs` ~l.1663) and in
`process_host::prepare` (~l.68): allowed iff `proc_spawn` or
`spawn_commands.contains(command)`. Exact string comparison, no `PATH`
resolution. The refusal is
`err("'<command>' is not in <option>; add it there to allow it")`.
`register-server` keeps requiring unbound `proc_spawn` — a language-server
registration is not what this grant was given for.

**Only user-authored values (security, found in review).** The grant ignores
the default the plugin registered: an option still at it grants no program and
is not remembered across a reload (`capability::Authorship::UserOnly`). A value
the user sets equal to that default is therefore ignored too — the price of
having no provenance on option values; set any other value, or hold it in
`lattice.toml`, where it counts.

**As built** (`f21e2d5a`): `Capability::ProcSpawnOption(SpawnOption)` in
`manifest.rs`; `CapabilityGrant { spawn_commands, spawn_options }` +
`may_spawn`, `GrantOutcome::spawn_bound` (`SpawnBoundGrant`) and
`option_commands` in `capability.rs`; the check in `process_host::prepare`; the
loader records `spawn_bound` and reloads on it. Tests: the OB.2 block in
`capability.rs`; `a_plugins_own_default_never_grants_a_program`,
`a_default_is_not_remembered_into_a_spawn_grant`,
`a_default_remembered_for_a_directory_grant_is_no_program`,
`a_held_languages_list_grants_its_programs` (`lib.rs`); two `prepare` tests in
`process_host.rs`; `tests/spawn_option_grant.rs` ×4; the loader's
`the_plugin_reloads_when_its_spawn_option_moves`.

**Beyond the plan:** the grammar-seam test is covered by the shared
check in `process_host::prepare` (unit-tested), not a grammar fixture;
`register-server` needs unbound `proc_spawn`, pinned by
`a_user_plugin_is_granted_exactly_the_configured_commands`; job cancel on
reload is the existing `PluginState`-owned-job teardown, not re-tested here.

**Reading a record list:** extend OB.1's closure so a `field` projection
over a list-of-records option yields that field's string from each record.
PMF.4 and PMF.5 cover the option exactly as in OB.1.

**Tests:**
- `a_user_plugin_spawns_a_listed_command` — a user-tier fixture whose option
  lists `sh` runs `sh -c 'echo hi'`, and `job-output` carries `hi`.
- `an_unlisted_command_is_refused_naming_the_option`.
- `an_absolute_path_does_not_match_a_bare_name` — Review focus #3.
- `the_default_empty_list_permits_nothing`.
- `unbound_proc_spawn_is_still_denied_to_user_plugins` — the old tier test.
- `register_server_still_needs_unbound_proc_spawn`.
- `editing_the_list_reloads_and_cancels_running_jobs` — a job outstanding at
  reload reports `job-finished(err("cancelled"))` (Review focus #5).
- Also on the grammar seam: `spawn_from_the_grammar_seam_obeys_the_option_grant`.

**Docs:** `docs/dev/architecture/plugin-host.md`'s capability table and the
generated plugin-API reference (`scripts/docs-check.sh` regenerates both).

### OB.3 — `post-effects` ✅ *(lattice)*

**WIT** (`crates/lattice-wit/wit/host-services.wit`):

```wit
/// Hand effects to the host from a seam that cannot return them — the
/// events seam's `on-event` and `on-wake`. They go through the host's
/// inbound path, which wakes the actor, so they paint without a keypress.
/// The same capability checks apply as to returned effects. `err` names
/// the first effect refused; effects before it were applied.
post-effects: func(effects: list<effect>) -> result<_, string>;
```

**Host:** implement on the host-services impl in `lattice-plugin-host`; route
through the inbound-bus primitive (`SubsystemBoot::inbound`'s plugin peer),
**not** a tick callback. Wire on both linkers. Additive to the package
(no guest that does not import it changes).

**Tests** (`crates/lattice-plugin-host/tests/`, with an events-guest
fixture):
- `posted_edit_is_visible_without_a_keypress` — the guest posts an
  `apply-edit` from `on-event(job-finished)`; assert the buffer text after
  settling the actor and with **no** dispatched action.
- `posted_effects_obey_the_grant` — a `write-to-file` outside `fs:write` is
  refused and nothing is written.
- `posting_to_a_closed_buffer_is_an_err_not_a_panic`.
- `a_guest_importing_post_effects_instantiates_on_both_linkers` — guards the
  unwired-import failure.

**As built** (`1cd970e5`): refused effects become a warning echo (as returned ones do) and
the call still answers `ok`; `err` means no sink was wired. The sink is set in
`lattice-plugin-loader::install` from `boot.inbound`, and `WiredSeams.effect_sink`
is the boot pin. "Without a keypress" is proven by composition — the post
reaching the sink (`post_effects_seam.rs`), the inbound wake (`lattice-mode`),
a drained `ApplyEdit` applying (`off_keystroke_applies_edits.rs`) — not by a new
full-editor plugin test. No WIT version bump here; OB.R1 decides crate versions.
Also: `PluginHost::set_effect_sink` / `effect_sink_wired`,
`PluginEffectSinkHandle`, `effect_from_guest` made `pub(crate)`; the events
fixture gained a `post-request` trigger and the `@example` for
`host-services.post-effects`; the plugin-API reference was regenerated
(`UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`).

### OB.R1 — lattice release 📝 *(lattice)*

A release carrying OB.1–OB.3, via `docs/dev/operations/releasing.md`. What it
must take care of, beyond the usual:

- **The plugin API crates.** OB.3 added `post-effects` to `host-services` and
  bumped nothing. `lattice-org-plugin` builds against the *published*
  `lattice-wit` / `lattice-plugin-sdk` / `lattice-plugin-sdk-derive`, so those
  must be published with the new WIT before OB.4 can call it. Choose the
  version per `crates/lattice-wit/README.md` (crate `major.minor` = the WIT
  package's); `the_crate_versions_track_the_wit_package_version` pins them
  together, so bump all three at once if the package version moves.
- **Built plugins go stale.** Any WIT edit moves `lattice_wit::ABI_FINGERPRINT`,
  so every plugin built against the old WIT is flagged stale until rebuilt —
  the bundled ones by `cargo xtask build-core-plugins`, user plugins on their
  next build. Say so in the release notes.
- **Release notes** name the user-visible change: user-installed plugins may
  now run programs you list in their options (`docs/user/plugins.md`,
  "What a plugin may run").

The plugin's `min_lattice` moves to this release in OB.4.

### OB.4 — `org-babel-mode` 📝 *(plugin)*

**Files:** create `src/babel/mod.rs` (mode registration, keymap, options);
modify `src/lib.rs` only to call `babel::register_*` from the existing
registration functions and to extend the `manifest!` (`provides` unchanged;
`default_modes` gains `"org-babel-mode"`; `capabilities` gains
`"fs:write:{org.babel.tangle-directories}"` and
`"proc:spawn:{org.babel.languages:command}"`; `min_lattice` to OB.R1).

**Mode:** a minor, `ActivationPolicy` naming org's major, keymap at its own
`MinorMode` layer, modelled on `org-table-mode` (lib.rs ~l.3131). Bindings
(design §2) registered against the mode's own action ids. `<C-c><C-c>`
returns `Effect::Declined` outside a `block` node whose `name` is `src`
(case-insensitive); the `<C-c><C-v>` chords never decline.

**Options** (`org.babel.*`): `enabled`, `confirm`, `languages` (list of
records `{ name, command, args: list<string>, ext, value: option<string> }`
through `config_shape::register_option`), `tangle-directories`
(`Vec<String>`), `min-lines-for-block-output`, `max-output-lines`.

In this slice the run action only echoes
`org: no babel language "<lang>" — add it to org.babel.languages` or
`org: running blocks lands in OB.6`, so the layer can be tested alone.

**Tests** (`integration/tests/org_babel.rs`, new):
- `babel_mode_is_active_in_an_org_buffer_by_default`.
- `ctrl_c_ctrl_c_outside_a_src_block_falls_through` — on a checkbox it still
  toggles; in a `#+begin_quote` block it reaches org's echo.
- `ctrl_c_ctrl_c_in_a_src_block_is_babels` — an unconfigured language echoes
  the "add it to org.babel.languages" message.
- `disabling_babel_removes_its_layer` — `org.babel.enabled = false`, then
  `C-c C-c` in a src block reaches org's "nothing to do here".
- `the_manifest_declares_babels_grants` — extend `org_manifest.rs`.

**Docs:** `:help org-babel` page stub through the help seam; `doc/org.md`
gains a Babel section listing the options.

### OB.5 — src blocks and header arguments 📝 *(plugin)*

**Files:** `src/babel/block.rs`, `src/babel/header_args.rs`.

**Interfaces (produced):**
- `SrcBlock { lang: String, name: Option<String>, header: HeaderArgs,
  body: String, begin_line: u32, end_line: u32, results: Option<LineRange> }`
  and `fn src_block_at(text, tree, line) -> Option<SrcBlock>` /
  `fn src_blocks(text, tree) -> Vec<SrcBlock>`.
- `HeaderArgs` — an ordered `key → Vec<String>` map with typed accessors for
  `results`, `dir`, `var`, `tangle`, `noweb`, `session`, `padline`, `shebang`.
- `fn effective_header_args(text, tree, block) -> HeaderArgs` — the merge
  order in design §5.1 (later wins).
- `fn block_key(block) -> BlockKey` — `#+NAME:` or `(lang, body hash,
  begin_line)` (design §5.3).

Results location: the `#+RESULTS:` (or `#+RESULTS[hash]:`) line after the
block, separated only by blank lines, or after `#+NAME:`-matched results
elsewhere; its extent is the following `: ` lines, table, drawer or
`#+begin_example…#+end_example`.

**Tests** (unit, in-module):
- header arguments parse `:results output table`, quoted values with spaces,
  repeated `:var`.
- the merge honours `#+PROPERTY: header-args`, `header-args:<lang>`, an
  inherited `:header-args:` drawer two headlines up, and the block line, in
  that order.
- results extent for each shape, including an emacs-written
  `#+RESULTS[3f2a…]:` (design §8, `:cache` read-only).
- `block_key` distinguishes two identical unnamed blocks by line.

**Bench:** `benches/babel_header_args.rs` — merge on a 5 000-line file with
the drawer six levels up; record in the plugin's bench notes and lattice's
`benchmarks.md` org section.

### OB.6 — `C-c C-c` runs a block 📝 *(plugin)*

**Files:** `src/babel/run.rs` (grammar-seam start), `src/babel/jobs.rs`
(events-seam accumulation and finish), `src/babel/results.rs` (formatting +
the edit).

**Start (grammar seam):** resolve the language record; write the body to
`/data/babel/<new-uuid>.<ext>`; substitute `{file}` with
`data-dir()/babel/<uuid>.<ext>`; `spawn-process`; store
`babel/job/<id>` → `{ buffer, block_key, header, script }` with `store-put`;
draw the running decoration (`refresh-decorations`). Refuse a block whose key
already has a live job, with an echo naming `:org-babel-cancel`.

**Finish (events seam):** subscribe to `job-output` and `job-finished` in
`register-events`. Accumulate lines per id, capped at `max-output-lines` with
`… N lines truncated`. On finish: read the store entry, re-read the buffer
with `source-line`, re-find the block by key, format, `post-effects([apply-edit])`
replacing the results extent (or inserting after `#+end_src`), clear the
decoration, delete the script and the store entry. Echo the `err` text
verbatim for a non-`ok` outcome. No key match ⇒ `output-append("*org-babel*",
…)` and an echo. `register-events` sweeps `/data/babel/`.

**Format (this slice):** verbatim only — `: ` lines below
`min-lines-for-block-output`, else `#+begin_example`, with `,`-escaping of
lines starting `*` or `#+` inside it.

**Tests** (`integration/tests/org_babel.rs`, with a configured `sh`):
- `a_block_runs_and_its_output_appears_without_a_keypress`.
- `existing_results_are_replaced_not_appended`.
- `output_that_looks_like_a_headline_cannot_become_one` — Review focus #1.
- `ten_lines_become_an_example_block`.
- `a_failing_program_still_writes_its_output_and_echoes_the_status`.
- `a_missing_interpreter_reports_the_spawn_error`.
- `editing_above_the_block_while_it_runs_still_lands_on_it`.
- `editing_the_block_body_while_it_runs_sends_output_to_org_babel` — Review
  focus #2; also `deleting_the_block…` and `closing_the_buffer…`.
- `two_identical_unnamed_blocks_each_get_their_own_result`.
- `a_running_block_is_not_started_twice`.
- `output_past_the_cap_is_truncated_with_a_count` — Review focus #4.
- `the_result_is_one_undo_step` — `u` restores the previous results exactly.
- `the_script_file_is_removed_after_the_run`.

### OB.7 — confirm, cancel, buffer and subtree 📝 *(plugin)*

**Files:** `src/babel/run.rs`, `src/babel/mod.rs`.

- Confirm: `effect.confirm { prompt, yes-action: "org-babel-run-confirmed",
  args: [<packed keys>] }` — keys packed into ONE slot. Skipped when
  `org.babel.confirm = false`. `execute-buffer` / `-subtree` ask once:
  `Run N blocks (sh ×3, python)? (y/n)`. Blocks run in document order, one
  after another (each waits for the previous `job-finished`), as babel does.
- `org-babel-cancel`: `cancel-job` for every live job of this buffer;
  their finish clears the decoration and leaves results untouched.
- `org-babel-remove-result`: one edit deleting the results extent.

**Tests:** `confirm_yes_runs_and_no_does_nothing`;
`confirm_carries_the_key_across_an_edit_made_while_the_prompt_is_open`;
`confirm_off_runs_at_once`; `execute_buffer_asks_once_and_runs_in_order`;
`execute_subtree_runs_only_its_blocks`; `cancel_kills_a_never_ending_block` —
`sleep 1000`, one finish, decoration gone (Review focus #4);
`remove_result_deletes_exactly_the_results`.

### OB.8 — `:results`, `value`, `:dir` 📝 *(plugin)*

**Files:** `src/babel/results.rs`, `src/babel/wrappers.rs`.

- Insertion: `replace`, `append`, `prepend`, `silent` (echo only), `none`.
- Format: `verbatim`, `table` (split on tabs, else runs of whitespace),
  `raw`, `drawer` (`:RESULTS:` … `:END:`), `code` (`#+begin_src <lang>`).
- `value`: built-in wrappers `python`, `ruby`, `node`, `sh`, selected by the
  language record's `value`; the wrapper prints the last expression's value
  after a sentinel line, and only what follows the sentinel is the result.
  No wrapper ⇒ `output`, with one echo per session saying so (design §5.2).
- `:dir`: tilde-expanded, passed as `cwd`; a non-directory is the spawn's
  error, surfaced as in OB.6.

**Tests:** one per insertion mode and per format; `value` for each wrapper
(skipped with a printed reason when the interpreter is absent on the CI
host); `value_without_a_wrapper_falls_back_to_output_and_says_so`;
`dir_with_a_tilde_runs_there`.

**Docs:** complete the Babel section of `doc/org.md`, `:help org-babel`, and
lattice's `docs/user/org.md`. Plugin release (`min_lattice` = OB.R1).

## Phase 2 — tangle

### OB.9 — `write-to-file` gains a `whole` anchor 📝 *(lattice)*

`file-anchor` (`types.wit` ~l.504) gains `whole`: replace the target buffer's
entire text as one edit, creating the file if absent. Same `fs:write` check;
`save` honoured as today. A target not already open is opened
`nobuflisted`. Adding a variant arm is an ABI break for guests that match
on `file-anchor` — follow the WIT versioning rule and say which version moves
in the commit.

**Tests:** `whole_replaces_an_open_buffer_as_one_undo_step`;
`whole_creates_an_absent_file`; `whole_with_save_reaches_disk`;
`whole_outside_the_grant_is_refused`; `an_unopened_target_is_not_listed`.
Then a lattice release (OB.R2).

### OB.10 — `:tangle` 📝 *(plugin)*

**Files:** `src/babel/tangle.rs`.

`:tangle yes` ⇒ the org file's stem + the language's `ext`; a path is
relative to the org file. Blocks to one target concatenate in document order;
`:padline no` drops the blank line between them; `:shebang` prefixes and then
`set-executable` on the target. **All-or-nothing:** `can-write-file` on every
target first; one refusal writes nothing and names the target and
`org.babel.tangle-directories`. Bound to `<C-c><C-v>t` and
`:org-babel-tangle`; echoes `Tangled N blocks to M files`.

**Tests:** `tangle_yes_names_the_file_after_the_org_file`;
`blocks_to_one_file_concatenate_in_order`; `padline_no`;
`shebang_sets_the_executable_bit`;
`one_disallowed_target_writes_nothing`;
`tangle_into_an_open_buffer_is_undoable`;
`file_wide_property_tangle_applies`.

## Phase 3 — cross-block

### OB.11 — `:var` 📝 *(plugin)*

**Files:** `src/babel/vars.rs`, `src/babel/wrappers.rs` (preludes).

Literals (`n=5`, `s="text"`), a named block's `#+RESULTS:` (verbatim ⇒ a
string, table ⇒ rows), and a named org table ⇒ rows. Preludes for `sh`
(strings; tables as newline/tab text), `python`, `ruby`, `node`. A language
without a prelude refuses `:var` by name. A reference to a name that does not
exist is an echoed error and nothing runs.

**Tests:** one per value kind per prelude language; quoting survives `'`,
`"`, `$` and newlines; `unknown_reference_runs_nothing`.

### OB.12 — dependency chains 📝 *(plugin)*

A `:var` reference whose block has no results runs that block first; the
referring block starts on its `job-finished`. The chain is a store-backed
state machine (crosses seams, design §5.3). Cycles over names are detected
before anything runs and echoed with the cycle. One confirm covers the chain.

**Tests:** `a_missing_result_is_computed_first`; `a_three_block_chain`;
`a_cycle_is_refused_before_anything_runs`;
`a_failing_dependency_stops_the_chain_and_says_where`.

### OB.13 — noweb 📝 *(plugin)*

**Files:** `src/babel/noweb.rs`.

`<<name>>` ⇒ the named block's body (recursive, cycle-checked);
`<<name()>>` ⇒ its result. `:noweb yes | no | tangle | no-export`, with
`tangle` expanding only when tangling. Applied in OB.6's start and OB.10's
tangle.

**Tests:** expansion, recursion, a cycle refused, indentation of the
reference line prefixed to every expanded line (as babel does),
`noweb_tangle_expands_only_when_tangling`.

## Phase 4 — sessions

### OB.14 — `spawn-interactive` and job stdin 📝 *(lattice)*

`host-services` gains `spawn-interactive` (same signature and gate as
`spawn-process`, stdin left open), `write-job-stdin(id, data: list<u8>) ->
result<_, string>` and `close-job-stdin(id)`. `spawn-process` is unchanged.
Writes to a finished or foreign job are `err`.

**Tests:** round-trip through `cat`; `write_after_exit_is_an_err`;
`another_plugins_job_is_not_writable`; cancel kills an interactive job's
tree. Then a lattice release (OB.R3).

### OB.15 — `:session` 📝 *(plugin)*

**Files:** `src/babel/session.rs`.

One interactive job per `(buffer, lang, session name)`, started on first use
via the language record (sessions need the interpreter in REPL mode, so the
record gains an optional `session-args`; a language without it refuses
`:session`). Each block is written to stdin followed by a per-language
sentinel echo; output up to the sentinel is that block's result. Sessions
are listed in `*org-babel*`, killed by `:org-babel-cancel`, on unload, and
when their buffer closes.

**Tests:** `state_persists_between_blocks_in_a_session`;
`two_sessions_are_independent`; `closing_the_buffer_ends_its_sessions`;
`a_block_that_never_prints_the_sentinel_can_be_cancelled`.
