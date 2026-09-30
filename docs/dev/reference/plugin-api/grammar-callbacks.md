<!-- @generated from wit/ by crates/lattice-plugin-api (render.rs).
     Do not edit: run `UPDATE_SITE_REFERENCE=1 cargo test -p lattice-plugin-api`. -->

# `grammar-callbacks`

**Direction:** guest implements this interface · **Capability:** none (pure data / dispatch) · **Worlds:** `auto-pair-plugin` (exports), `comment-plugin` (exports), `grammar-plugin` (exports), `project-plugin` (exports), `treesitter-context-plugin` (exports)

The behavior callbacks a grammar plugin **exports**; the host calls one by
`callback` id on dispatch (the PH7.3d callback-id trampoline). **Synchronous**
— a grammar `apply` resolves on the keystroke path (the PH7.7 fork: a motion
must return inline to compose with its operator; async would break
operator∘motion atomicity + dot-repeat/macros). Each maps its native
evaluator's `GrammarResult<...>`: `ok` is the produced value; an `err` string
is logged and the contribution is a no-op (graceful degradation, §8). A trap
(fuel/epoch) is the runaway guard — the host catches it, logs, and the
contribution no-ops, never a hang (a Reflex-class budget bounds it, PH7.7c).

An operator/ex-command/action returns `list<effect>` — the boundary form of
the closed `Effect` enum (`Effect::Many` flattens to the list; §4.4). A text
object returns the `range` it resolved; a motion its `motion-result`.

## Uses

- [`motion-context`](types.md#record-motion-context) from [`types`](types.md)
- [`motion-result`](types.md#record-motion-result) from [`types`](types.md)
- [`operator-context`](types.md#record-operator-context) from [`types`](types.md)
- [`text-object-context`](types.md#record-text-object-context) from [`types`](types.md)
- [`ex-command-context`](types.md#record-ex-command-context) from [`types`](types.md)
- [`action-context`](types.md#record-action-context) from [`types`](types.md)
- [`range`](types.md#record-range) from [`types`](types.md)
- [`effect`](types.md#variant-effect) from [`types`](types.md)
- [`args`](types.md#variant-args) from [`types`](types.md)
- [`document`](buffer.md#resource-document) from [`buffer`](buffer.md)
- [`tree-snapshot`](tree-sitter.md#resource-tree-snapshot) from [`tree-sitter`](tree-sitter.md)

## Functions (6)

### `apply-action`

```wit
apply-action: func(callback: u32, ctx: action-context, doc: borrow<document>, tree: option<borrow<tree-snapshot>>) -> result<list<effect>, string>
```

### `apply-ex-command`

```wit
apply-ex-command: func(callback: u32, ctx: ex-command-context, doc: borrow<document>, tree: option<borrow<tree-snapshot>>) -> result<list<effect>, string>
```

OC.10 gave this `doc` and `tree`, so a plugin ex-command can read the
buffer it was invoked from — the same pair `apply-action` receives, minted
at the same instant so their versions agree (§7). `tree` is `none` for a
plain-text buffer, a parse still in flight, or a plugin without the
`tree-sitter` grant.

### `apply-motion`

```wit
apply-motion: func(callback: u32, ctx: motion-context, doc: borrow<document>, tree: option<borrow<tree-snapshot>>) -> result<motion-result, string>
```

OM.4: a motion receives `borrow<document>` too. The `apply-action`
doc-comment below anticipated this — "text-reading motions (structural /
word motions) can reuse the same handle when a motion signature needs
it" — and org's headline motions (`]]` / `[[` / `g{`) are the first that
do: finding the next headline means reading lines.

OT.1: a motion receives the tree too, on the same terms as an action —
acquired the same instant as `doc`, `none` when the buffer has no parse.

This doc-comment used to say a motion gets the document but NOT the tree,
because "the native `MotionContext` carries a `ScopeResolver` rather than
a `SyntaxSnapshot`, so there is no tree handle to mint here without
changing the native context — and no motion has yet needed one. When one
does, that is the slice that adds it." Org's headline motions are that
motion: they resolve `(section)` / `(headline)` structure, and hand-rolled
star-counting is what OT.x exists to end.

The native change was smaller than that paragraph predicted.
`GrammarEnv::syntax` already carried the type-erased snapshot on every
dispatch — `execute_action` cloned it into `ActionContext` and the motion
and text-object contexts simply never read it. So this cost two borrowed
fields, not new plumbing. Borrowed rather than cloned because motions fire
on every `j`: a native motion pays nothing, and only a plugin motion that
actually mints the resource pays the `Arc` bump.

### `apply-operator`

```wit
apply-operator: func(callback: u32, ctx: operator-context, doc: borrow<document>) -> result<list<effect>, string>
```

CM.1: an operator receives `borrow<document>`, the pair the motion,
text-object, action and ex-command callbacks already had (AP.0.1,
OM.4b, OT.1, OC.10). It was the last one without, because no plugin had
contributed an operator — and a comment operator cannot work without
the text: comment-vs-uncomment, the indent column, and stripping an
existing leader are all reads.

**No `tree`, deliberately.** `OperatorContext` carries `document` and
`comment_syntax` but, unlike `TextObjectContext`, no `path` or
`syntax` — minting a tree resource would mean widening the native
context and every operator call site for a capability no operator has
asked for. Add it when one does; the asymmetry is a decision, not an
oversight.

**Example — A linewise operator that reads the range and returns edits for the host to apply** · [`plugins/comment/src/lib.rs`](../../../../plugins/comment/src/lib.rs)

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

### `apply-text-object`

```wit
apply-text-object: func(callback: u32, ctx: text-object-context, doc: borrow<document>, tree: option<borrow<tree-snapshot>>) -> result<range, string>
```

OM.4b: a text object receives `borrow<document>` too — `text-object-context`
has always said "buffer text + the scope/comment env ride the `document`
handle", and AP.0.1 simply wired the action path first. Org's headline
and subtree objects are the first plugin ones, and resolving a subtree's
bounds means reading lines.

OT.1: and the tree, for the `apply-motion` reason above — org's `ir` / `ar`
resolve a subtree, which IS the `(section)` node. A text object gets the
tree rather than only the `scope-resolver` the native structural objects
use, because the resolver answers "what encloses this point" while a
plugin object needs to query the tree itself.

### `parse-ex-args`

```wit
parse-ex-args: func(callback: u32, rest: string, bang: bool) -> result<args, string>
```

