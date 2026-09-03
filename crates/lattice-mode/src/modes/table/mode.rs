//! TB.1 — `table-mode`: the shared minor that edits pipe tables.
//!
//! Design: [`table-mode.md`](../../../../../../docs/dev/architecture/table-mode.md).
//! Slice plan: [`table-mode.md`](../../../../../../docs/dev/operations/slice-plans/table-mode.md).
//!
//! ## Why this is native and shared, not org's
//!
//! Pipe tables are markdown's and org's alike, so by "shared behaviour is a
//! minor mode, never a copied keymap" the chords belong in ONE mode spanning
//! both majors. The question that decides *where* is which owner can serve
//! both, and only the host can: `markdown-mode` is a native major, so a table
//! mode living in the org plugin would make markdown table editing require
//! the org plugin installed and enabled — silently absent otherwise, since a
//! chord nobody bound simply does nothing.
//!
//! The host was already the owner in fact. [`super::layout`] has been here
//! since HP.1, its module doc naming "a `table-mode` in this directory" as
//! its next consumer, and it measures cells by display width where the
//! plugin's copy counted `char`s — a difference its own doc called "honestly
//! wrong for CJK". Two engines existed; this slice keeps the better one.
//!
//! `org-table-mode` stays, and is not redundant: it is where behaviour that
//! is genuinely org's goes — `#+TBLFM:` formulas above all. What it no longer
//! carries is the generic surface every pipe table shares.
//!
//! ## Layering: these chords decline, they do not swallow
//!
//! `<Tab>` in a table advances a cell; anywhere else it must still mean what
//! it meant. So the bodies return [`Effect::Declined`] when the caret is not
//! in a table, and the dispatcher re-resolves the chord against the layers
//! below — `org-mode`'s headline cycle, then the builtin jump-forward. Two
//! hops, and the chain is a tested property rather than an assumed one.
//!
//! **`Declined` is right only for a chord shared with a lower layer.** A
//! decline re-runs a multi-key chord's TRAILING key alone, so declining
//! `<leader>tK` outside a table would fire a bare `K`. The `<leader>t…`
//! family returns [`Effect::None`] instead: nothing below binds them, and
//! consuming the chord is the honest no-op. See `decline-only-shared-chords`.

use std::sync::{Arc, OnceLock};

use lattice_grammar::effect::Effect;
use lattice_grammar::registry::{ActionContext, ActionSpec};
use lattice_grammar::{CommandRegistry, GrammarResult};
use lattice_protocol::edit::{Edit, EditKind};
use lattice_protocol::position::{Position, Range};

use super::edit::{Cell, column_at, offset_of_column};
use super::model::Table;
use crate::registry::ModeRegistry;
use crate::{
    ActivationPolicy, Keymap, KeymapEntry, LifecycleFuture, Mode, ModeContext, ModeId, ModeKind,
    keymap_entry,
};

pub const ALIGN: &str = "action:table-align";
pub const NEXT_CELL: &str = "action:table-next-cell";
pub const PREV_CELL: &str = "action:table-prev-cell";
pub const ROW_UP: &str = "action:table-row-up";
pub const ROW_DOWN: &str = "action:table-row-down";
pub const COLUMN_LEFT: &str = "action:table-column-left";
pub const COLUMN_RIGHT: &str = "action:table-column-right";
pub const INSERT_ROW: &str = "action:table-insert-row";
pub const INSERT_COLUMN: &str = "action:table-insert-column";
pub const DELETE_ROW: &str = "action:table-delete-row";
pub const DELETE_COLUMN: &str = "action:table-delete-column";
// TB.3
pub const INSERT_RULE: &str = "action:table-insert-rule";
pub const SORT: &str = "action:table-sort";
pub const SORT_DESC: &str = "action:table-sort-descending";
pub const BLANK_CELL: &str = "action:table-blank-cell";
pub const COPY_DOWN: &str = "action:table-copy-down";
pub const TRANSPOSE: &str = "action:table-transpose";
// TB.4 — Insert-mode only.
pub const NEXT_ROW: &str = "action:table-next-row";
pub const REALIGN: &str = "action:table-realign";

/// `table-mode` — pipe-table editing, on every major that has pipe tables.
pub struct TableMode;

impl TableMode {
    pub fn mode_id() -> ModeId {
        ModeId::new("table-mode")
    }
}

impl Mode for TableMode {
    type Guard = ();

    fn id(&self) -> ModeId {
        Self::mode_id()
    }

    fn kind(&self) -> ModeKind {
        ModeKind::Minor
    }

    /// The majors that have pipe tables.
    ///
    /// `org-mode` is a PLUGIN major, and naming it here is deliberate rather
    /// than a layering slip: the policy matches on a mode id at activation
    /// time, so the host needs no knowledge of the plugin beyond the string —
    /// and the alternative, org declaring the relationship from its side,
    /// would mean the mode's activation surface lived in two repos.
    ///
    /// Not `Always`: a table mode on a Rust buffer would take `<Tab>` from
    /// completion the moment a line started with `|`, and a match arm does.
    fn activation_policy(&self) -> ActivationPolicy {
        ActivationPolicy::Majors(vec![ModeId::new("markdown-mode"), ModeId::new("org-mode")])
    }

    fn keymap(&self) -> Keymap {
        Keymap::from_entries(table_keymap_entries())
    }

    fn on_activate(&self, _ctx: ModeContext) -> LifecycleFuture<'_, ()> {
        Box::pin(async { Ok(()) })
    }
}

/// The chords, inherited from the mode this takes over from.
///
/// `<leader>t…` and the directional letters are org's set verbatim, because
/// the users who have them should not have to relearn them for the same
/// operations — and `K`/`J`/`H`/`L` keep one mnemonic across subtrees and
/// table rows.
fn table_keymap_entries() -> &'static [KeymapEntry] {
    static ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();
    ENTRIES.get_or_init(|| {
        vec![
            keymap_entry! { mode: Normal, chord: "<Tab>", doc: "Next table cell", cmd: Some(NEXT_CELL) },
            keymap_entry! { mode: Normal, chord: "<S-Tab>", doc: "Previous table cell", cmd: Some(PREV_CELL) },
            keymap_entry! { mode: Normal, chord: "<leader>t|", doc: "Align this table", cmd: Some(ALIGN) },
            keymap_entry! { mode: Normal, chord: "<leader>tK", doc: "Move table row up", cmd: Some(ROW_UP) },
            keymap_entry! { mode: Normal, chord: "<leader>tJ", doc: "Move table row down", cmd: Some(ROW_DOWN) },
            keymap_entry! { mode: Normal, chord: "<leader>tH", doc: "Move table column left", cmd: Some(COLUMN_LEFT) },
            keymap_entry! { mode: Normal, chord: "<leader>tL", doc: "Move table column right", cmd: Some(COLUMN_RIGHT) },
            keymap_entry! { mode: Normal, chord: "<leader>tr", doc: "Insert a table row below", cmd: Some(INSERT_ROW) },
            keymap_entry! { mode: Normal, chord: "<leader>tc", doc: "Insert a table column right", cmd: Some(INSERT_COLUMN) },
            keymap_entry! { mode: Normal, chord: "<leader>tdr", doc: "Delete this table row", cmd: Some(DELETE_ROW) },
            keymap_entry! { mode: Normal, chord: "<leader>tdc", doc: "Delete this table column", cmd: Some(DELETE_COLUMN) },
            // TB.3. `-` for a rule is emacs' own mnemonic (`C-c -`), and the
            // rest read as what they do rather than as emacs' chord letters:
            // this mode is not org's, so `C-c ^` for sort has no claim here.
            keymap_entry! { mode: Normal, chord: "<leader>t-", doc: "Insert a table rule below", cmd: Some(INSERT_RULE) },
            keymap_entry! { mode: Normal, chord: "<leader>ts", doc: "Sort this section by the column at the cursor", cmd: Some(SORT) },
            keymap_entry! { mode: Normal, chord: "<leader>tS", doc: "Sort this section descending", cmd: Some(SORT_DESC) },
            keymap_entry! { mode: Normal, chord: "<leader>tb", doc: "Blank the table cell at the cursor", cmd: Some(BLANK_CELL) },
            keymap_entry! { mode: Normal, chord: "<leader>ty", doc: "Copy this cell down, incrementing a trailing number", cmd: Some(COPY_DOWN) },
            keymap_entry! { mode: Normal, chord: "<leader>tT", doc: "Transpose this table", cmd: Some(TRANSPOSE) },

            // ── TB.4: the same keys, in Insert, meaning what a table wants
            // there. A chord resolves per `BindingMode`, so this is the model
            // working rather than a special case — a table is a thing you
            // navigate in Normal and fill in in Insert.
            //
            // `<Tab>` and `<S-Tab>` reuse the Normal BODIES verbatim: moving
            // to a cell and realigning is the same operation, and neither
            // emits `Effect::EnterMode`, so Insert is simply kept. Writing a
            // second pair of handlers to say that would be the duplication
            // this mode exists to end.
            keymap_entry! { mode: Insert, chord: "<Tab>", doc: "Next table cell", cmd: Some(NEXT_CELL) },
            keymap_entry! { mode: Insert, chord: "<S-Tab>", doc: "Previous table cell", cmd: Some(PREV_CELL) },
            // `<CR>` is bound in Insert ONLY, and the asymmetry is the point.
            // In Normal it already means something a table row wants — the
            // first non-blank of the next line. In Insert it means "split this
            // line", which inside a table row is never what you meant.
            keymap_entry! { mode: Insert, chord: "<CR>", doc: "Next table row, same column", cmd: Some(NEXT_ROW) },
            // The chord that makes this feel like "as you type": fill a cell,
            // press `<Esc>`, the table snaps. `fall_through` so the mode
            // realigns and the dispatcher then continues to whatever `<Esc>`
            // natively means — the mode never hardcodes exit-insert.
            // `active-snippet-mode`'s `<Esc>` is the precedent (SN.3c.2b).
            keymap_entry! {
                mode: Insert,
                chord: "<Esc>",
                doc: "Realign this table, then exit insert (falls through to the native <Esc>)",
                cmd: Some(REALIGN),
                fall_through: true
            },
        ]
    })
}

pub fn register_table_mode(registry: &mut ModeRegistry) {
    registry
        .register(TableMode)
        .expect("table-mode must register without conflict");
}

// ── The bodies ──────────────────────────────────────────────────────────

/// What a chord found when it fired: the table, the caret's cell, and the
/// line the caret was on.
struct Located {
    table: Table,
    cell: Cell,
}

fn locate(ctx: &ActionContext) -> Option<Located> {
    let line_count = ctx.buffer.rope_line_count();
    let table = Table::at(|n| ctx.buffer.line(n), ctx.cursor.line, line_count)?;
    let row = table.row_index(ctx.cursor.line)?;
    let text = ctx.buffer.line(ctx.cursor.line)?;
    let column = column_at(&text, ctx.cursor.byte as usize);
    Some(Located {
        table,
        cell: Cell { row, column },
    })
}

/// Replace the table's lines with `rendered` and park the caret in `cell`.
///
/// **One edit over the whole span.** A column insert touches every line, and
/// a half-applied column is a corrupt table — worse than either end state.
/// It also means `u` undoes the operation rather than its last row.
fn rewrite(ctx: &ActionContext, table: &Table, rendered: Vec<String>, cell: Cell) -> Effect {
    // **The replaced range is the OLD table's span, end to end.** Deriving the
    // end line from `rendered` instead is the bug this comment exists to stop
    // coming back: for align and the two swaps the row count is unchanged and
    // the two agree, so it looks right — and then an insert addresses a line
    // past the buffer's end and does nothing at all, while a delete leaves the
    // last row behind. The org plugin's integration suite caught it on
    // `<leader>tr`; the unit tests here did not, because they assert the
    // rendered TEXT and never the range it lands in.
    let caret_line = table.first + cell.row as u32;
    let caret_col = rendered
        .get(cell.row)
        .map(|l| offset_of_column(l, cell.column) as u32)
        .unwrap_or(0);
    // TB.4: **no edit when nothing changed.** Without this, `<Tab>` through an
    // already-aligned table pushes an undo entry per cell and `<Esc>` pushes
    // one every time you leave Insert inside a table — undo steps for edits
    // that changed nothing, which is worse than the papercut it looks like:
    // it makes `u` stop meaning "undo my last change". The caret still has to
    // move, so this is a cursor-only effect rather than `None`.
    let unchanged = rendered.len() as u32 == table.last - table.first + 1
        && rendered
            .iter()
            .enumerate()
            .all(|(i, l)| ctx.buffer.line(table.first + i as u32).as_deref() == Some(l.as_str()));
    if unchanged {
        return Effect::CursorMove(Position::new(caret_line, caret_col));
    }
    let end_byte = ctx
        .buffer
        .line(table.last)
        .map(|l| l.len() as u32)
        .unwrap_or(0);
    let text = rendered.join("\n");
    Effect::ApplyEdit {
        target: ctx.buffer_id,
        edit: Edit {
            range: Range::new(
                Position::new(table.first, 0),
                Position::new(table.last, end_byte),
            ),
            kind: EditKind::Replace { text },
        },
        cursor: Some(Position::new(caret_line, caret_col)),
    }
}

/// A body that needs a table under the caret, sharing one decline policy.
///
/// `shared` says whether the chord is one a lower layer also binds. It
/// decides what "not in a table" returns, and getting it wrong is not
/// cosmetic: `Declined` re-runs a multi-key chord's trailing key ALONE, so
/// declining `<leader>tK` would fire a bare `K`.
fn in_table(
    shared: bool,
    f: impl Fn(&ActionContext, Located) -> Option<Effect> + Send + Sync + 'static,
) -> Arc<dyn Fn(&ActionContext) -> GrammarResult<Effect> + Send + Sync> {
    let declined = if shared {
        Effect::Declined
    } else {
        Effect::None
    };
    Arc::new(move |ctx: &ActionContext| {
        let Some(found) = locate(ctx) else {
            return Ok(declined.clone());
        };
        // An operation that cannot apply (a row at the edge, the last
        // column) consumes the chord rather than declining: the caret IS in a
        // table, so falling through to a headline cycle would be a surprise.
        Ok(f(ctx, found).unwrap_or(Effect::None))
    })
}

fn spec(
    shared: bool,
    f: impl Fn(&ActionContext, Located) -> Option<Effect> + Send + Sync + 'static,
) -> ActionSpec {
    ActionSpec {
        apply: in_table(shared, f),
        args_schema: vec![],
    }
}

/// Move the caret to `cell` without changing the text — what `<Tab>` does
/// when the table is already aligned.
fn move_to(ctx: &ActionContext, table: &Table, cell: Cell) -> Option<Effect> {
    let rendered = table.render();
    Some(rewrite(ctx, table, rendered, cell))
}

/// Register every `table-mode` action.
///
/// The mode owns its keymap AND these bodies — the standing rule, and the gap
/// `magit-project-diff` shipped through when it declared one without the
/// other.
pub fn register_table_actions(registry: &mut CommandRegistry) {
    registry.register_action(
        ALIGN,
        "Align the columns of the table at the cursor.",
        spec(false, |ctx, found| {
            let rendered = found.table.render();
            Some(rewrite(ctx, &found.table, rendered, found.cell))
        }),
    );
    // `<Tab>` and `<S-Tab>` are SHARED — org's headline cycle and the builtin
    // jump-list sit below them — so these two decline.
    registry.register_action(
        NEXT_CELL,
        "Move to the next cell of the table at the cursor, aligning it.",
        spec(true, |ctx, found| {
            // TB.3: at the last cell, `<Tab>` ADDS a row and lands in it.
            // Emacs' org-table does, and it is how people actually build a
            // table — type a row, Tab, type the next. `Table::next_cell`
            // deliberately answers `None` there rather than inventing this,
            // because "add a row" and "leave the table" are both defensible
            // and the choice belongs to the surface, not the model.
            match found.table.next_cell(found.cell) {
                Some(next) => move_to(ctx, &found.table, next),
                None => {
                    let last = found.table.rows.len().saturating_sub(1);
                    let (next, cell) = found.table.insert_row(Cell {
                        row: last,
                        column: found.cell.column,
                    });
                    let rendered = next.render();
                    Some(rewrite(ctx, &found.table, rendered, cell))
                }
            }
        }),
    );
    registry.register_action(
        PREV_CELL,
        "Move to the previous cell of the table at the cursor, aligning it.",
        spec(true, |ctx, found| {
            let prev = found.table.prev_cell(found.cell)?;
            move_to(ctx, &found.table, prev)
        }),
    );
    for (name, doc, delta) in [
        (ROW_UP, "Move this table row up.", -1isize),
        (ROW_DOWN, "Move this table row down.", 1),
    ] {
        registry.register_action(
            name,
            doc,
            spec(false, move |ctx, found| {
                let (next, cell) = found.table.move_row(found.cell, delta)?;
                let rendered = next.render();
                Some(rewrite(ctx, &found.table, rendered, cell))
            }),
        );
    }
    for (name, doc, delta) in [
        (COLUMN_LEFT, "Move this table column left.", -1isize),
        (COLUMN_RIGHT, "Move this table column right.", 1),
    ] {
        registry.register_action(
            name,
            doc,
            spec(false, move |ctx, found| {
                let (next, cell) = found.table.move_column(found.cell, delta)?;
                let rendered = next.render();
                Some(rewrite(ctx, &found.table, rendered, cell))
            }),
        );
    }
    registry.register_action(
        INSERT_ROW,
        "Insert an empty table row below the cursor.",
        spec(false, |ctx, found| {
            let (next, cell) = found.table.insert_row(found.cell);
            let rendered = next.render();
            Some(rewrite(ctx, &found.table, rendered, cell))
        }),
    );
    registry.register_action(
        INSERT_COLUMN,
        "Insert an empty table column right of the cursor.",
        spec(false, |ctx, found| {
            let (next, cell) = found.table.insert_column(found.cell);
            let rendered = next.render();
            Some(rewrite(ctx, &found.table, rendered, cell))
        }),
    );
    registry.register_action(
        DELETE_ROW,
        "Delete the table row at the cursor.",
        spec(false, |ctx, found| {
            let (next, cell) = found.table.delete_row(found.cell)?;
            let rendered = next.render();
            Some(rewrite(ctx, &found.table, rendered, cell))
        }),
    );
    registry.register_action(
        DELETE_COLUMN,
        "Delete the table column at the cursor.",
        spec(false, |ctx, found| {
            let (next, cell) = found.table.delete_column(found.cell)?;
            let rendered = next.render();
            Some(rewrite(ctx, &found.table, rendered, cell))
        }),
    );

    // ── TB.3 ────────────────────────────────────────────────────────────
    registry.register_action(
        INSERT_RULE,
        "Insert a horizontal rule below the cursor's table row.",
        spec(false, |ctx, found| {
            let (next, cell) = found.table.insert_rule(
                found.cell,
                rule_fallback(ctx.path.as_deref().map(|p| p.as_path())),
            );
            let rendered = next.render();
            Some(rewrite(ctx, &found.table, rendered, cell))
        }),
    );
    for (name, doc, descending) in [
        (
            SORT,
            "Sort this table section by the column at the cursor.",
            false,
        ),
        (
            SORT_DESC,
            "Sort this table section by the column at the cursor, descending.",
            true,
        ),
    ] {
        registry.register_action(
            name,
            doc,
            spec(false, move |ctx, found| {
                let (next, cell) = found.table.sort_section(found.cell, descending)?;
                let rendered = next.render();
                Some(rewrite(ctx, &found.table, rendered, cell))
            }),
        );
    }
    registry.register_action(
        BLANK_CELL,
        "Empty the table cell at the cursor.",
        spec(false, |ctx, found| {
            let (next, cell) = found.table.blank_cell(found.cell)?;
            let rendered = next.render();
            Some(rewrite(ctx, &found.table, rendered, cell))
        }),
    );
    registry.register_action(
        COPY_DOWN,
        "Copy this cell into the row below, incrementing a trailing number.",
        spec(false, |ctx, found| {
            let (next, cell) = found.table.copy_down(found.cell)?;
            let rendered = next.render();
            Some(rewrite(ctx, &found.table, rendered, cell))
        }),
    );
    registry.register_action(
        TRANSPOSE,
        "Swap the table's rows and columns.",
        spec(false, |ctx, found| {
            let (next, cell) = found.table.transpose(found.cell)?;
            let rendered = next.render();
            Some(rewrite(ctx, &found.table, rendered, cell))
        }),
    );

    // ── TB.4 ────────────────────────────────────────────────────────────
    // `<CR>` is SHARED with the native newline, so it declines outside a
    // table — consuming it would break `<CR>` in every markdown and org
    // buffer, which is a far larger surface than this mode.
    registry.register_action(
        NEXT_ROW,
        "Move to the row below in the same table column, adding a row at the end.",
        spec(true, |ctx, found| {
            match found.table.down_cell(found.cell) {
                Some(down) => move_to(ctx, &found.table, down),
                None => {
                    // Emacs' `org-table-next-row` creates a row at the bottom
                    // rather than stopping, which is what makes `<CR>` the way
                    // you enter a table's contents. You leave with `<Esc>`.
                    let (next, cell) = found.table.insert_row(found.cell);
                    let rendered = next.render();
                    Some(rewrite(ctx, &found.table, rendered, cell))
                }
            }
        }),
    );
    // `<Esc>` is shared too — and `fall_through` means the native exit-insert
    // runs after this either way; declining is what keeps `<Esc>` working in
    // the far more common case of not being in a table at all.
    registry.register_action(
        REALIGN,
        "Realign the table at the cursor.",
        spec(true, |ctx, found| {
            let rendered = found.table.render();
            Some(rewrite(ctx, &found.table, rendered, found.cell))
        }),
    );
}

/// Which rule style a table with NO rule should get: `+` for org, `|` for
/// everything else.
///
/// This is the single place the dialect is not readable off the buffer — §2
/// of the design says the table says which dialect it is, and a table with no
/// rule has not said. So the file's extension answers, which is crude but
/// true: the thing that decides whether `|---+---|` is idiomatic IS whether
/// this is an org file.
///
/// Not a `table.dialect` option, for §2's reason — an option is a second
/// source that can disagree with the buffer. Not the buffer's major mode
/// either, tempting as that is: reaching it from a grammar action body means
/// either a `lattice-syntax` dependency in `lattice-mode` or a new field on
/// `ActionContext`, which is a large edge for one character. A path with no
/// extension, or none at all, gets markdown's — the more common table
/// spelling, and the one org accepts too.
fn rule_fallback(path: Option<&std::path::Path>) -> char {
    let is_org = path
        .and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("org"));
    if is_org { '+' } else { '|' }
}
