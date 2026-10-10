# Plugin patterns

Recipes for the things plugins actually do: add an operator, bind an action,
own a mode, contribute a picker, react to events, read the buffer and its
syntax tree, remember state. Each pattern names the world to target, the
entry point that registers the contribution, the callback that does the work,
and the traps.

Every code block here is **quoted from a plugin or test guest that CI
compiles** against the current WIT — most of them also run under real
guest↔host tests. A test (`crates/lattice-plugin-api/tests/guides.rs`) fails
when a quoted region and this page disagree, so what you copy is what builds
today. For exact signatures and every type, follow the links into the
[plugin-API reference](../reference/plugin-api.md); for the toolchain, the ABI
and how plugins are built and loaded, read the
[authoring guide](plugin-authoring.md) first.

## The shape of every plugin

A plugin is three things.

**A world.** The WIT world says which seams the component imports (host
functions it may call) and exports (callbacks the host calls on it). A plugin
contributing to several seams declares its own world composing them — the
`comment-plugin` world imports `grammar`, `buffer`, `tree-sitter`, `modes`,
`config` and `help`, and exports `grammar-callbacks`. Every world, with what it
imports and exports, is on the [worlds page](../reference/plugin-api/worlds.md).

**Entry points.** Each contribution world exports `register-*` functions —
`register-grammar`, `register-modes`, `register-options`,
`register-help-topics`, `register-picker-sources`, … The host calls each once,
at load, and the plugin declares what it contributes by calling host imports
from inside them. Work happens later, in the callbacks the host invokes.

**A manifest.** `plugin.toml` names the plugin, lists the seams it `provides`,
and requests capabilities. This is `comment`'s — every comment in it is there
for a reason worth reading:

<!-- include: plugins/comment/plugin.toml -->
```toml
# CM.3 — the `comment` bundled plugin manifest.
#
# `capabilities = ["grammar:chord"]` is the CM.2 capability: permission to bind
# an operator's chord into the universal operator-pending grammar. It is
# declared rather than assumed because claiming keys in the grammar every
# buffer shares is the most user-visible power a plugin can take — the claim
# belongs in the manifest, in `:plugins`' capability column, and in the grant
# the trust tier computes. Withheld, the operator still registers and stays
# reachable by name; only `gc` stops working.
#
# No `fs:` or `state:write`: the operator reads the buffer through the
# `borrow<document>` handle it is handed and returns edits. It touches nothing
# else.
id = "comment"
provides = ["grammar", "modes", "config", "help"]
capabilities = ["grammar:chord"]

# CM.2: the chord is scoped to this mode's keymap layer, never `Builtin`. A
# chord at `Builtin` would outlive `:set comment.enabled=false` and point at a
# handler that is gone. The loader refuses to bind a chord for a plugin with no
# `default_modes` for exactly that reason.
default_modes = ["comment-mode"]
```

Its `Cargo.toml` is the minimal component crate: a `cdylib`, `wit-bindgen`,
and a standalone `[workspace]` so it never inherits the host's target or lints:

<!-- include: plugins/comment/Cargo.toml -->
```toml
# CM.3 — the `comment` bundled plugin. `gc{motion}` / `gcc` / Visual `gc`,
# contributed from WASM.
#
# Chosen as the first new plugin for what it proves rather than what it does:
# paramount goal #3 says adding operators is first-class, and nothing
# demonstrates that like an OPERATOR crossing the boundary and composing with
# every motion and text object without a line in `lattice-grammar`.
#
# Standalone workspace (`[workspace]` below): targets wasm32-wasip2, must not
# inherit the host toolchain's lints/target, and must not be built by a plain
# `cargo build --workspace`. Built by `cargo xtask build-core-plugins`.
[package]
name = "comment"
version = "0.0.0"
edition = "2021"
publish = false

[lib]
crate-type = ["cdylib"]

[dependencies]
wit-bindgen = "0.58"

[profile.release]
opt-level = "s"
strip = true

[workspace]
```

## An operator

**Target:** a world exporting `register-grammar` and `grammar-callbacks`
(`comment-plugin` is the template). **Register** in `register-grammar` with
`grammar.register-operator`, giving it a callback id; **implement**
`grammar-callbacks.apply-operator`, which the host calls with that id.

<!-- example: comment:grammar.register-operator -->
```rust
grammar::register_operator(
    "comment-toggle",
    "toggle line comments over the operated range",
    &OperatorSpec {
        repeatable: true,
        args_schema: Vec::new(),
        // `false`, and load-bearing: a blockwise `<C-v>` selection
        // arrives as ONE contiguous range rather than per row, like
        // `>` / `gU` and unlike `d` / `y`. Rule 1 is a property of the
        // range — decided per row, a mixed block inverts. See
        // `toggle::tests::a_mixed_block_must_be_decided_as_one_range`.
        blockwise_per_row: false,
        post_motion_char: false,
        // CM.2: the chord travels with the operator. `doubled` is the
        // TRAILING key, so this is `gcc` — the spelling commentary and
        // Neovim use — rather than `gcgc`.
        chord: Some("gc".to_string()),
        doubled: Some("c".to_string()),
    },
    CB_TOGGLE,
);
```

The chord is optional. Binding one requires the `grammar:chord` capability in
the manifest; withheld, the operator still registers and stays reachable by
name, only the keys are not bound. The chord lands in the plugin's own mode's
keymap layer, so disabling the mode removes the keys with it.

The callback receives the range the motion or text object resolved to, and a
`borrow<document>` to read the text. It **returns effects** — here, one
`apply-edit` per changed line — and the host applies them. A guest never
mutates a buffer directly.

<!-- example: comment:grammar-callbacks.apply-operator -->
```rust
fn apply_operator(
    callback: u32,
    ctx: OperatorContext,
    doc: &Document,
) -> Result<Vec<Effect>, String> {
    if callback != CB_TOGGLE {
        return Err(format!("comment: unknown operator callback {callback}"));
    }

    // The grammar hands over an expanded range; the operator is linewise
    // regardless of how the motion arrived, which is what `gc$` doing the
    // whole line means.
    let first = ctx.range.start.line;
    let last = ctx.range.end.line;

    // Graceful and specific: the echo names the reason. A silent no-op
    // here is the failure mode the plugin-host rules keep legislating
    // against — the user presses `gc`, nothing happens, and nothing says
    // why.
    let path = doc.path();
    let Some(leader) = path.as_deref().and_then(toggle::leader_for_path) else {
        return Ok(vec![Effect::Echo(lattice::plugin_host::types::EchoPayload {
            level: lattice::plugin_host::types::EchoLevel::Warn,
            text: match path.as_deref() {
                None => "comment: this buffer has no file, so no comment syntax".to_string(),
                Some(p) => format!("comment: no comment syntax known for `{p}`"),
            },
        })]);
    };

    let mut nums = Vec::new();
    let mut texts = Vec::new();
    for n in first..=last {
        if let Some(text) = doc.line(n) {
            nums.push(n);
            texts.push(text);
        }
    }

    // `leader-space` is read per invocation rather than cached: a plugin
    // that snapshots an option at load answers from the value the user had
    // when the editor started, forever.
    // Read per invocation, not cached: a plugin that snapshots an option
    // at load answers from the value the user had when the editor started,
    // forever. `auto-pair::is_manual` reads its own option the same way.
    // Absent or unparseable ⇒ the registered default, `true`.
    let leader_space = config::get_option("leader-space")
        .map(|v| v != "false")
        .unwrap_or(true);

    let mut edits = Vec::new();
    for (i, next) in toggle::toggle(&texts, leader, leader_space)
        .into_iter()
        .enumerate()
    {
        // `None` means the line is unchanged — no edit, so a no-op `gc`
        // stays off the undo stack.
        let Some(next) = next else { continue };
        edits.push(Effect::ApplyEdit(ApplyEditPayload {
            // CM.3: the buffer the operator ran over. A guest holds a
            // read-only handle, so it asks the host to apply rather than
            // mutating — which is why `operator-context` had to carry an
            // id at all.
            target: ctx.buffer_id,
            edit: Edit {
                range: Range {
                    start: Position {
                        line: nums[i],
                        byte: 0,
                    },
                    end: Position {
                        line: nums[i],
                        byte: texts[i].len() as u32,
                    },
                },
                kind: EditKind::Replace(next),
            },
            // Leave the caret where the user put it; vim's `gc` does not
            // move it.
            cursor: None,
        }));
    }
    Ok(edits)
}
```

**Traps.** Grammar callbacks run **synchronously on the keystroke**, on the
sync linker, under a small per-call fuel budget. Keep them fast, never block,
and do not use `std::fs` inside them — it panics on that linker; use
`host-services.read-file` (see
[Sync or async](plugin-authoring.md#sync-or-async-and-why-it-matters)).
Return an `err` rather than trapping: a trap quarantines the whole plugin.

## An action bound to keys

An action is a command with no range. Register it with
`grammar.register-action` and handle it in `grammar-callbacks.apply-action`.
`auto-pair` registers one action per chord from a table:

<!-- example: auto-pair:grammar.register-action -->
```rust
let spec = || ActionSpec {
    args_schema: Vec::new(),
};
for (name, doc, cb) in [
    ("auto-pair-open-round", "insert ()", CB_OPEN_ROUND),
    ("auto-pair-open-square", "insert []", CB_OPEN_SQUARE),
    ("auto-pair-open-curly", "insert {}", CB_OPEN_CURLY),
    ("auto-pair-close-round", "step over )", CB_CLOSE_ROUND),
    ("auto-pair-close-square", "step over ]", CB_CLOSE_SQUARE),
    ("auto-pair-close-curly", "step over }", CB_CLOSE_CURLY),
    ("auto-pair-quote-double", "pair \"\"", CB_QUOTE_DOUBLE),
    ("auto-pair-quote-single", "pair ''", CB_QUOTE_SINGLE),
    ("auto-pair-quote-backtick", "pair ``", CB_QUOTE_BACKTICK),
    (
        "auto-pair-close-manual",
        "close the nearest unmatched opener in scope (manual style)",
        CB_CLOSE_MANUAL,
    ),
    (
        "auto-pair-backspace",
        "delete an empty pair, else fall through to normal backspace",
        CB_BACKSPACE,
    ),
] {
    grammar::register_action(name, doc, &spec(), cb);
}
```

Returning the `declined` effect **falls through**: the dispatcher re-resolves
the chord as if this binding were not there, so `auto-pair` can decline a
keystroke it does not want and let ordinary insertion happen.

<!-- example: auto-pair:grammar-callbacks.apply-action -->
```rust
fn apply_action(
    callback: u32,
    ctx: ActionContext,
    doc: &Document,
    tree: Option<&TreeSnapshot>,
) -> Result<Vec<Effect>, String> {
    // AP.3: in `manual` style the pair keys (1..=9) self-insert — the action
    // DECLINES so the typed char lands via the builtin, and only the close key
    // + backspace act. In `auto` style the close key declines instead.
    let manual = is_manual();
    if manual && (CB_OPEN_ROUND..=CB_QUOTE_BACKTICK).contains(&callback) {
        return Ok(vec![Effect::Declined]);
    }
    Ok(match callback {
        CB_OPEN_ROUND => insert_pair(&ctx, "(", ")"),
        CB_OPEN_SQUARE => insert_pair(&ctx, "[", "]"),
        CB_OPEN_CURLY => insert_pair(&ctx, "{", "}"),
        CB_CLOSE_ROUND => close(&ctx, doc, ")"),
        CB_CLOSE_SQUARE => close(&ctx, doc, "]"),
        CB_CLOSE_CURLY => close(&ctx, doc, "}"),
        CB_QUOTE_DOUBLE => quote(&ctx, doc, "\""),
        CB_QUOTE_SINGLE => quote(&ctx, doc, "'"),
        CB_QUOTE_BACKTICK => quote(&ctx, doc, "`"),
        // The manual close key acts only in `manual` style; in `auto` it
        // declines so `<C-j>` does whatever else it's bound to.
        CB_CLOSE_MANUAL if manual => manual_close(&ctx, doc, tree),
        CB_CLOSE_MANUAL => vec![Effect::Declined],
        CB_BACKSPACE => backspace(&ctx, doc),
        other => return Err(format!("auto-pair: unknown action callback {other}")),
    })
}
```

## A motion or a text object

Same seam, two more registrations. A motion answers *where the cursor goes*;
a text object answers *which range*. Both compose with every operator — a
plugin motion after `d` deletes to where it lands.

<!-- example: grammar-guest:grammar.register-motion -->
```rust
grammar::register_motion(
    "down-n",
    "jump count lines down (fixture)",
    &MotionSpec {
        jump: false,
        exclusive: false,
        args_schema: Vec::new(),
    },
    1,
);
```

<!-- example: multiseam-guest:grammar-callbacks.apply-motion -->
```rust
fn apply_motion(
    c: u32,
    _ctx: MotionContext,
    _doc: &Document,
    tree: Option<&TreeSnapshot>,
) -> Result<MotionResult, String> {
    match c {
        // OT.1: target the end of the parse tree's own span. Unanswerable
        // without the tree, so `none` surfaces as a guest err rather than a
        // wrong-but-believable line.
        20 => {
            let tree = tree.ok_or_else(|| "multiseam: motion got no tree".to_string())?;
            Ok(MotionResult {
                target: tree.root().byte_range().end,
                linewise: true,
            })
        }
        other => Err(format!("multiseam: unknown motion callback {other}")),
    }
}
```

<!-- example: grammar-guest:grammar.register-text-object -->
```rust
grammar::register_text_object(
    "to-cursor",
    "line start to cursor (fixture)",
    &TextObjectSpec {
        args_schema: Vec::new(),
    },
    2,
);
```

<!-- example: grammar-guest:grammar-callbacks.apply-text-object -->
```rust
fn apply_text_object(
    callback: u32,
    ctx: TextObjectContext,
    _doc: &Document,
    _tree: Option<&TreeSnapshot>,
) -> Result<Range, String> {
    match callback {
        2 => Ok(Range {
            start: Position {
                line: ctx.at.line,
                byte: 0,
            },
            end: ctx.at,
        }),
        other => Err(format!("fixture: unknown text-object callback {other}")),
    }
}
```

## An ex-command

Register with `grammar.register-ex-command`. Two callbacks answer it:
`grammar-callbacks.parse-ex-args` turns the raw text after the command name
into typed `args` (an `err` is echoed to the user before anything runs), and
`grammar-callbacks.apply-ex-command` does the work.

<!-- example: project:grammar.register-ex-command -->
```rust
lattice::plugin_host::grammar::register_ex_command(
    "project-switch",
    "Choose a project, then act on it. The verb this whole plugin \
     exists for: every other project-aware surface roots itself at the \
     buffer you are standing in, which is right until you want the one \
     you are not.",
    &ExCommandSpec {
        latency_class: LatencyClass::Reflex,
        accepts_bang: false,
        accepts_range: false,
        args_schema: Vec::new(),
        surface_form: SurfaceForm::Keyword,
    },
    CB_PARSE,
    CB_SWITCH,
);
```

<!-- example: project:grammar-callbacks.parse-ex-args -->
```rust
fn parse_ex_args(_c: u32, rest: String, _bang: bool) -> Result<Args, String> {
    let rest = rest.trim();
    Ok(if rest.is_empty() {
        Args::None
    } else {
        Args::String(rest.to_string())
    })
}
```

<!-- example: multiseam-guest:grammar-callbacks.apply-ex-command -->
```rust
fn apply_ex_command(
    c: u32,
    ctx: ExCommandContext,
    doc: &Document,
    tree: Option<&TreeSnapshot>,
) -> Result<Vec<Effect>, String> {
    if c == 31 {
        // Everything here was unreachable before OC.10: `ctx.cursor` says
        // which line, `ctx.buffer_id` names the target, `doc` proves the
        // buffer is readable, and `tree` proves the parse crossed too. The
        // echo reports the last two so a regression to the old context fails
        // loudly rather than editing the right line for the wrong reason.
        let had_line = doc.line(ctx.cursor.line).is_some();
        let kind = tree.map(|t| t.root().kind()).unwrap_or_else(|| "none".into());
        let old = doc.line(ctx.cursor.line).unwrap_or_default();
        return Ok(vec![
            Effect::ApplyEdit(ApplyEditPayload {
                target: ctx.buffer_id,
                edit: Edit {
                    range: Range {
                        start: Position { line: ctx.cursor.line, byte: 0 },
                        end: Position {
                            line: ctx.cursor.line,
                            byte: old.len() as u32,
                        },
                    },
                    kind: EditKind::Replace(format!("EX:{had_line}:{kind}")),
                },
                cursor: None,
            }),
        ]);
    }
    Err("multiseam: no ex-commands".into())
}
```

## A mode that owns your surface, and its options

A plugin's chords, options and behaviour belong to a **mode** it declares —
so turning the mode off turns the feature off, and unloading the plugin
takes everything with it. Declare it in `register-modes`:

<!-- example: comment:modes.register-mode -->
```rust
modes::register_mode(&ModeDeclaration {
    id: "comment-mode".to_string(),
    kind: ModeKind::Minor,
    // `global`, not `universal`: every DOCUMENT buffer. `gc` over
    // user-edited text is the point; `gc` in `*messages*`, the file
    // tree or a help popup is noise.
    activation_policy: ActivationPolicy::Global,
    capabilities: ModeCapabilities::empty(),
    // Not language-scoped: `gc` works in every document buffer, and
    // which leader to use is decided per-buffer from the path.
    target_language: None,
    // No option overrides — the mode changes how keys behave, not how
    // its buffers behave.
    options: Vec::new(),
    // No keymap here. The operator's chord is bound by the host into
    // the operator-pending layer (CM.2) — a plain binding could not
    // give `gc` a motion, and would kill `gcc` besides.
    keymap: Vec::new(),
});
```

List the mode in the manifest's `default_modes` and the loader registers a
`<plugin-id>.enabled` option that gates it, on by default.

Options the plugin owns are registered in `register-options` through
`config.register-option`. The host namespaces the name by plugin id, so this
becomes `comment.leader-space`, settable with `:set` like any built-in option:

<!-- example: comment:config.register-option -->
```rust
config::register_option(
    "leader-space",
    OptionType::Boolean,
    "true",
    "insert a space between the comment leader and the code (`// x`, not `//x`)",
);
```

**Read an option at the moment you use it**, not once at load — a plugin that
caches it answers from the value the user had when the editor started:

<!-- example: auto-pair:config.get-option -->
```rust
/// Read the live style option (AP.3). `auto` (default) or `manual`. The plugin
/// uses the SHORT name `style`; the host auto-namespaces it to `auto-pair.style`
/// (the name a user sets). The grammar guest reads the SHARED editor config
/// registry (wired at instantiate time), so `:set auto-pair.style=manual` flips
/// behavior live — no keymap re-registration.
fn is_manual() -> bool {
    config::get_option("style").as_deref() == Some("manual")
}
```

For options with structure — a list of records — use
`config.register-structured-option` and `config.get-option-value`:

<!-- example: project:config.register-structured-option -->
```rust
let rows = switch::defaults();
let _ = lattice::plugin_host::config::register_structured_option(
    switch::OPTION,
    &switch::schema(),
    &switch::to_value(&rows),
    "Rows of the project-switch menu. Each names an ex-command that \
     takes a project root as its first argument — which is the whole \
     contract for adding your own.",
);
```

## A picker

**Target:** a world exporting `register-picker-sources` and `picker-source`.
In `register-picker-sources`, declare each source with
`picker-registry.register-picker-source`; one component may register several.

<!-- example: picker-guest:picker-registry.register-picker-source -->
```rust
fn register_picker_sources() {
    register_picker_source(&PickerSourceSpec {
        id: FIXTURE.to_string(),
        doc: "PH7.4c.1b fixture picker source".to_string(),
        args_schema: Vec::new(),
        args_hint: "[fail]".to_string(),
        live: false,
        // OR.5: the source declares that it can create what the query
        // names. `%s` is replaced by the query when the row renders.
        create_label: Some("Create fixture: %s".to_string()),
        // PP.2: `true` on purpose. The field's failure mode is a boundary
        // arm that writes the default, which a fixture declaring `false`
        // cannot tell apart from one that carries the value — the hole
        // PC.11's `fill-action` shipped through.
        rooted: true,
        // PD.1, same reasoning as `rooted` above and the same hole it
        // guards: a boundary arm writing `None` is indistinguishable from
        // one that carried a `None`, so this source names a command and
        // its sibling names none.
        delete_command: Some("fixture-forget".to_string()),
    });
    register_picker_source(&PickerSourceSpec {
        id: SECOND.to_string(),
        doc: "OR.5b: a SECOND source from the same component".to_string(),
        args_schema: Vec::new(),
        args_hint: String::new(),
        live: false,
        create_label: None,
        // …and `false` here, so the pair proves the value TRAVELS rather
        // than that the host defaults everything to the same answer.
        rooted: false,
        delete_command: None,
    });
}
```

`picker-source.init` builds the rows for the source the user opened. Each row
pairs a candidate with an opaque **routing** token the picker never reads:

<!-- example: project:picker-source.init -->
```rust
/// `source` is checked rather than assumed: one component may register
/// several sources and they share one actor, so a source id this plugin
/// never registered is untrusted input, not a case to fall through.
fn init(
    source: String,
    ctx: PickerContext,
    _args: Vec<String>,
) -> Result<Vec<CandidatePair>, String> {
    let pairs = match source.as_str() {
        picker::PROJECTS_PICKER => picker::init(load())?,
        // PB.1: the root rides the CONTEXT, not the args — PC.1's rule,
        // and the same reason: `:project-buffers` opened from the
        // switch-commands menu names a project other than the one the
        // buffer is in, and `Effect::OpenPicker { root }` is the seam that
        // carries it. Reading `args[0]` would work for this source and
        // then be a second convention for the next one.
        picker::PROJECT_BUFFERS_PICKER => picker::buffers_init(
            &ctx.workspace_root,
            ctx.buffers,
            ctx.active_buffer.buffer_id,
        ),
        other => return Err(format!("project: no picker source `{other}`")),
    };
    Ok(pairs
        .into_iter()
        .map(|(candidate, routing)| CandidatePair { candidate, routing })
        .collect())
}
```

When the user accepts a row, `picker-source.accept` gets that token back and
turns it into the outcome the host performs — open a file, switch buffer,
jump, run a command:

<!-- example: picker-guest:picker-source.accept -->
```rust
fn accept(
    source: String,
    _ctx: PickerContext,
    routing: RoutingPayload,
) -> Result<PickerAcceptOutcome, String> {
    // OR.5b: the second source's accept is distinguishable too — otherwise a
    // test could not tell "routed to the right source" from "there is only
    // one body".
    if source == SECOND {
        return Ok(PickerAcceptOutcome::OpenFile("/second/accepted".to_string()));
    }
    match routing {
        RoutingPayload::OpenFile(p) => Ok(PickerAcceptOutcome::OpenFile(p)),
        RoutingPayload::Buffer(id) => Ok(PickerAcceptOutcome::SwitchBuffer(id)),
        // OR.5: the create row. The query crosses VERBATIM — the host must
        // not have trimmed, lowercased or otherwise had an opinion about a
        // namespace it does not own — so the fixture echoes it back inside
        // a path the test can compare exactly.
        RoutingPayload::Create(query) => {
            Ok(PickerAcceptOutcome::OpenFile(format!("/created/{query}")))
        }
        _ => Err("fixture: unexpected routing token".to_string()),
    }
}
```

Picker callbacks run on the **async** linker, off the keystroke, so they may
take longer and may use `std::fs` within the plugin's grant.

## Reacting to events and time

A world exporting `register-events`, `on-event` and `on-wake` subscribes in
`register-events` and handles deliveries in `on-event`. Handlers run off the
hot path.

<!-- example: project:events.subscribe -->
```rust
/// Subscribe to `document-opened` — how a project comes to be remembered at
/// all, and `project.el`'s `project-remember-project` in one line.
///
/// Filtered to the one kind rather than taking everything and branching: the
/// filter is the host's, so an unfiltered subscription would wake this
/// plugin's task for every modal-mode change and every option write in the
/// editor, to do nothing.
fn register_events() {
    lattice::plugin_host::events::subscribe(
        &EventFilter {
            kinds: Some(vec![EventKind::DocumentOpened]),
            path_globs: None,
            major_modes: None,
            minor_modes: None,
        },
        ON_DOCUMENT_OPENED,
    );
}

/// Runs on the event actor's own task, never a keystroke — which is the
/// property that lets it do a store read+write at all.
///
/// Silent by construction: a handler that echoed would announce a project on
/// every file you open. A store failure is dropped here rather than shown,
/// because there is no user action that provoked it and nothing they could
/// do about it mid-open; `:project-remember` is the path that reports.
fn on_event(handler: u32, ev: Event) {
    if handler != ON_DOCUMENT_OPENED {
        return;
    }
    let Event::DocumentOpened(opened) = ev else {
        return;
    };
    // A buffer with no path on disk resolves to `pwd`, which
    // `project_of_buffer` already refuses — but checking here avoids a host
    // call per scratch buffer, and the field is right there.
    if opened.path.is_none() {
        return;
    }
    // `opened.id` is a `DocumentId` by TYPE and a buffer id by VALUE:
    // `publish_document_opened_for_active` builds it as
    // `DocumentId::new(buffer_id.0 as u64)`. `root-for-buffer` wants the
    // buffer id, so passing this straight through is correct — verified
    // rather than assumed, because the two type names disagree and a wrong
    // id here would resolve to `none` and silently remember nothing.
    if let Some(root) = project_of_buffer(opened.id) {
        let _ = remember_root(&root);
    }
}
```

For periodic work, arm a wake and keep its id; cancel it when done:

<!-- example: events-guest:events.wake-every -->
```rust
// OC.2: arm a periodic wake from registration. 50 ms is the seam's
// floor — fast enough that a test does not sit on a real clock, and the
// guest cancels itself after a few fires so it cannot run away.
wake_state::TICKER.with(|t| t.set(events::wake_every(50)));
```

<!-- example: events-guest:events.cancel-wake -->
```rust
let n = wake_state::FIRES.with(|f| {
    let n = f.get() + 1;
    f.set(n);
    n
});
record(&format!("wake:{n}"));
if n >= wake_state::CANCEL_AFTER {
    events::cancel_wake(id);
}
```

To be told when files change, watch a directory. Batches arrive as the
`files-changed` event, addressed only to the plugin that armed the watch:

<!-- example: events-guest:host-services.watch -->
```rust
events::subscribe(&kind_filter(EventKind::FilesChanged), 6);
let outcome = match host_services::watch(target) {
    Ok(()) => "watch:ok".to_string(),
    Err(e) => format!("watch:err({e})"),
};
record(&outcome);
```

To fetch a file, ask the host to download it. The call returns an id at once
and never the bytes: the host streams to disk on its own thread, verifies the
SHA-256 you pinned, and tells you how it went with a `job-finished` event
carrying that id. A failed download — wrong hash, refused redirect, cancelled — leaves
nothing at the destination, so there is no cleanup to write:

<!-- example: events-guest:host-services.http-download -->
```rust
events::subscribe(&kind_filter(EventKind::JobProgress), 8);
events::subscribe(&kind_filter(EventKind::JobFinished), 8);
let outcome = match host_services::http_download(url, sha256, dest) {
    // The id is what `job-finished` will carry; a plugin running
    // several jobs keys its state by it.
    Ok(_id) => "download:started".to_string(),
    Err(e) => format!("download:err({e})"),
};
record(&outcome);
```

It needs two grants: `net:http:<host>` for the URL's host (and for every host
a redirect passes through — a release URL that bounces to a CDN needs both),
and `fs:write` over the destination.

Unpacking is a job too, reported by the same `job-finished` event — the host
confines every entry to the destination and leaves nothing behind on failure:

<!-- example: events-guest:host-services.extract-archive -->
```rust
events::subscribe(&kind_filter(EventKind::JobFinished), 9);
let outcome = match host_services::extract_archive(src, dest, format) {
    Ok(_id) => "extract:started".to_string(),
    Err(e) => format!("extract:err({e})"),
};
record(&outcome);
```

A bare `.gz`, or a binary downloaded as-is, arrives with no executable bit and
a plugin cannot set one itself. Ask the host once the job has succeeded:

<!-- example: events-guest:host-services.set-executable -->
```rust
let outcome = match host_services::set_executable(dest) {
    Ok(()) => "set-executable:ok".to_string(),
    Err(e) => format!("set-executable:err({e})"),
};
record(&outcome);
```

A bundled plugin can also run a program — a package manager, for a tool with
no pre-built binary. Its output arrives as `job-output` events, a batch of
lines at a time, and its exit as `job-finished`. User-installed plugins are
refused: a subprocess is not sandboxed.

<!-- example: events-guest:host-services.spawn-process -->
```rust
events::subscribe(&kind_filter(EventKind::JobOutput), 10);
events::subscribe(&kind_filter(EventKind::JobFinished), 10);
// No shell: each element of `args` is one argument, whatever it
// contains. `""` runs it in the editor's working directory.
let outcome = match host_services::spawn_process(command, &args, "") {
    Ok(_id) => "spawn:started".to_string(),
    Err(e) => format!("spawn:err({e})"),
};
record(&outcome);
```

Having installed a language server, a bundled plugin tells the editor to use
it. The registration replaces any server the editor already knew under the same
id, starts nothing by itself — the next matching buffer does — and is withdrawn
automatically when the plugin unloads:

<!-- example: events-guest:host-services.register-server -->
```rust
let config = host_services::ServerConfig {
    id: id.to_string(),
    // An absolute path into the install tree — no `PATH` entry
    // needed, which is the point of managing the install.
    command: command.to_string(),
    args: vec!["--stdio".to_string()],
    env: Vec::new(),
    root_markers: vec![".git".to_string()],
    file_patterns: vec![pattern.to_string()],
    language_id: id.to_string(),
    initialization_options: None,
};
let registered = host_services::register_server(&config);
```

A downloaded program has to match the machine. Your plugin is `wasm32`
wherever it runs, so ask:

<!-- example: events-guest:host-services.host-platform -->
```rust
let platform = host_services::host_platform();
let build = format!("{}-{}", platform.os, platform.arch);
```

And it has to go somewhere. The calls above take paths on the host, where your
`/data` means nothing; `data-dir` is that directory's real path, and everything
under it is yours to use with no `fs:` capability in the manifest:

<!-- example: events-guest:host-services.data-dir -->
```rust
// …and named to the host by its real path. No `fs:` capability
// is needed for anything under this directory.
let outcome = match host_services::data_dir() {
    Some(dir) => host_services::set_executable(&format!("{dir}/tool"))
        .map(|()| dir),
    None => Err("no data dir".to_string()),
};
```

To show any of this to the user, write to an **output buffer**: a read-only
buffer that follows its last line, which your plugin fills and the editor
displays. It works from any export, including `on-event`, which cannot return
an effect. Name the buffer in the `*name*` form; the lines are kept whether or
not anyone has it open:

<!-- example: events-guest:host-services.output-append -->
```rust
let appended = host_services::output_append(
    name,
    &[
        "resolving rust-analyzer".to_string(),
        // One string, two lines: the host splits on newlines.
        "downloading\nverifying".to_string(),
    ],
);
```

To put the buffer on screen, return `Effect::OpenSyntheticBuffer` from a
command, with the same `name` and `mode_id: "plugin-output-mode"`. Opening and
writing can happen in either order.

The row pinned at the top is the headerline. Use it for where the work is, and
how it ended — each call replaces the last:

<!-- example: events-guest:host-services.output-status -->
```rust
let status = host_services::output_status(
    name,
    host_services::OutputState::Running,
    "downloading\u{2026} 43%",
);
```

Before a second run, empty the buffer so the new output does not land under
the old:

<!-- example: events-guest:host-services.output-reset -->
```rust
let reset = host_services::output_reset(name);
```

## Reading the buffer and the syntax tree

Callbacks that need text get a `borrow<document>`: a snapshot, so a
concurrent edit never shifts ranges under you mid-read. Read only what you
need — a line, or a byte range:

<!-- example: grammar-guest:buffer.document.line -->
```rust
let line = doc
    .line(ctx.range.start.line)
    .ok_or_else(|| format!("fixture: no line {}", ctx.range.start.line))?;
// The PATH as well as the text. An operator's handle was minted
// with `path: None` at first, so `document.path()` answered
// `none` for every real file — invisible until a plugin asked.
let path = doc.path().unwrap_or_else(|| "<none>".to_string());
Ok(vec![Effect::Echo(EchoPayload {
    level: EchoLevel::Info,
    text: format!("op|{path}|{line}"),
})])
```

<!-- example: auto-pair:buffer.document.get-text-range -->
```rust
/// The single byte after the caret (empty string at EOL / on a read error —
/// which just means "nothing to step over", so insert).
fn char_after(ctx: &ActionContext, doc: &Document) -> String {
    doc.get_text_range(Range {
        start: ctx.cursor,
        end: one_right(ctx.cursor),
    })
    .unwrap_or_default()
}
```

Callbacks that take an `option<borrow<tree-snapshot>>` can query the parse
tree. Compile a query once per language, run it, and read the captures —
predicates are evaluated host-side:

<!-- example: treesitter-context:tree-sitter.tree-snapshot.compile-query -->
```rust
let Some(source) = query_for(&language) else {
    // No query for this grammar. Not an error — the strip simply has
    // nothing to show, and the host caches that as "no scopes".
    return Ok(Vec::new());
};
// Compiled per call rather than cached: the guest has no per-language
// cache slot that survives a call, and this runs once per REPARSE (not per
// keystroke, scroll, or frame), so the cost sits far off every hot path.
// A cache would be the right move only if the producer were re-driven more
// often, and the whole scopes-not-rows split exists to ensure it is not.
let query = tree.compile_query(source)?;
```

<!-- example: treesitter-context:tree-sitter.tree-snapshot.run-query-ranges -->
```rust
// `run_query_ranges`, not `run_query`: this is a WHOLE-FILE structural
// query, and the node-returning form pays a resource handle per capture.
// See the module doc — that difference is the file-size ceiling.
let captures = tree.run_query_ranges(&query, None);
let mut scopes: Vec<ContextScope> = Vec::new();
// Captures arrive grouped by match (the host pushes each match's captures
// together and stamps them with one index), so one linear scan pairs each
// `@context` with its `@context.end` — no containment test, which would be
// ambiguous for a construct nested directly inside another.
let mut i = 0;
while i < captures.len() {
    let match_index = captures[i].match_index;
    let mut extent: Option<(u32, u32)> = None;
    let mut body_start: Option<u32> = None;
    while i < captures.len() && captures[i].match_index == match_index {
        let c = &captures[i];
        match c.name.as_str() {
            "context" => extent = Some((c.range.start.line, c.range.end.line)),
            "context.end" => body_start = Some(c.range.start.line),
            // A query may carry captures for its own predicates; anything
            // unrecognised is ignored rather than treated as a scope.
            _ => {}
        }
        i += 1;
    }
    if let Some(extent) = extent {
        scopes.push(scope_from(extent, body_start));
    }
}
// A scope spanning a single line can never be a context: its header cannot
// scroll away while the cursor is still inside it. Dropping them here keeps
// the host's cache (and the resolver's scan) free of entries that can never
// resolve to anything.
scopes.retain(|s| s.scope_end > s.scope_start);
Ok(scopes)
```

The tree may be absent (no grammar for the file, or not parsed yet), so
every tree-driven callback needs a path for `none`.

## Remembering state across restarts

`host-services.store-put` and `host-services.store-get` persist bytes under a
plugin-private key. The store needs the `state:write` capability —
deliberately separate from `fs:write`, so remembering something does not
require a grant over the user's files. Treat an absent key as a fresh install:

<!-- example: project:host-services.store-get -->
```rust
/// Read the remembered list.
///
/// A `none` from `store-get` covers every degraded case — no grant, no data
/// dir, a store discarded as corrupt — and the seam's own doc says a reader for
/// whom absence is ordinary cannot distinguish them and does not need to. Here
/// absence genuinely is ordinary: it is a fresh install.
fn load() -> Vec<String> {
    host_services::store_get(STORE_KEY)
        .map(|bytes| projects::decode(&bytes))
        .unwrap_or_default()
}
```

<!-- example: project:host-services.store-put -->
```rust
/// Persist the list. The `Err` is returned rather than swallowed so a command
/// can echo it — a `:project-remember` that reports success and stored nothing
/// is precisely the silent failure this plugin must not have.
fn save(list: &[String]) -> Result<(), String> {
    host_services::store_put(STORE_KEY, &projects::encode(list))
}
```

## Shipping help, and logging

Register your `:help` pages in `register-help-topics`; embed the markdown at
build time so the page always matches the build:

<!-- example: comment:help.register-topic -->
```rust
let _ = help::register_topic(
    "",
    "Toggle line comments with `gc` — an operator, so it takes any motion or text object.",
    include_str!("../doc/comment.md"),
    &["comment".to_string()],
);
```

Async-world guests can narrate their work into the plugin trace buffer
(`:plugin-trace`), gated by the plugin's `plugin.trace-level`:

<!-- example: logging-guest:logging.log -->
```rust
// Distinct levels + contexts so the host test can assert routing, level
// mapping, and the context→category rendering. `info`/`warn` are kept at
// the default gate; `debug`/`trace` only when the plugin is raised.
logging::log(Level::Info, "boot", "logging guest activated");
logging::log(Level::Warn, "index", "reindex found 2 stale entries");
logging::log(Level::Debug, "detail", "walked 40 files in 3ms");
logging::log(Level::Error, "", "a context-less error line");
```

## Rules that apply to every pattern

- **Errors are values.** Functions return `result<_, string>`; the message
  reaches the user, so say what went wrong and what to do. A trap — panic,
  out of fuel, past the deadline — quarantines the plugin until reload.
- **Sync seams are on the keystroke.** `grammar`, `error-parser` and
  `dashboard` rendering run synchronously; everything else is async. The
  difference decides whether `std::fs` works (it does not on the sync linker)
  and how much work a call may do.
- **Capabilities are deny-by-default.** Request only what the plugin uses;
  the grant is the intersection of the request and the trust tier.
- **Effects, not mutation.** Callbacks describe what should happen by
  returning effects; the host applies them, in order, on its own thread.

For what each function takes and returns, and what every field means, the
[reference](../reference/plugin-api.md) is generated from the WIT and is
always current.
