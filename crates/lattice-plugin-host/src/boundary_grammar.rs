//! The grammar-extension boundary conversions (plugin-host.md §4.1, PH7.7a).
//!
//! Mirrors the data a plugin authors against when it EXTENDS the vim grammar
//! via `register_{motion,operator,text_object,ex_command,action}`. The grammar
//! *handling* (dispatcher, parser, composition) stays native + sync + untouched
//! (PH7.7 fork, locked): a plugin only contributes entries; it never observes
//! or reimplements dispatch.
//!
//! Two directions, matching the picker seam (`boundary_picker.rs`):
//!   - **Contexts** are host→guest one-way projections of the dispatch
//!     environment (`project_*` free fns — the contexts carry `&Buffer` /
//!     `&CancellationToken` / `Option<&dyn ScopeResolver>` borrows, so they
//!     cannot round-trip; the guest never sends a context back). Bulk buffer
//!     text never rides a context — it crosses via the `buffer` `document`
//!     resource handle (§4.2), so a projection reads only the owned scalars.
//!   - **Results** come back guest→host: `MotionResult` here; a text object
//!     returns `range` (`NativeRange::from_wit`), an operator/ex-command returns
//!     `effect` (`NativeEffect::from_wit`), `parse_args` returns `args`
//!     (`NativeArgs::from_wit`) — all reusing the PH7.3b conversions.
//!
//! The contribution *spec* records (`motion-spec`/…) mirror each native `*Spec`
//! with the `apply` / `parse_args` closure dropped — the behavior is a sync
//! guest export the host calls back by callback-id (PH7.7b/c), not a field that
//! crosses. Their scalar fields reuse the conversions this module adds
//! (`LatencyClass`, `SurfaceForm`) plus the existing `ArgSpec` mirror; the
//! WIT-record → native-`*Spec` direction is PH7.7c's trampoline job (it needs
//! the callback closure), so no spec `from_wit` lands here.

use crate::WitBoundary;
use crate::lattice::plugin_host::types::{
    ActionContext as WitActionContext, ExCommandContext as WitExCommandContext,
    LatencyClass as WitLatencyClass, MotionContext as WitMotionContext,
    MotionResult as WitMotionResult, OperatorContext as WitOperatorContext,
    SurfaceForm as WitSurfaceForm, TextObjectContext as WitTextObjectContext,
};
use lattice_grammar::command::LatencyClass as NativeLatencyClass;
use lattice_grammar::registry::{
    ActionContext as NativeActionContext, ExCommandContext as NativeExCommandContext,
    MotionContext as NativeMotionContext, MotionResult as NativeMotionResult,
    OperatorContext as NativeOperatorContext, SurfaceForm as NativeSurfaceForm,
    TextObjectContext as NativeTextObjectContext,
};

/// Intern a plugin-supplied owned string as `&'static str`. `SurfaceForm`'s
/// `Delimiter { hint }` is `&'static str` (a native builtin uses a literal); a
/// plugin supplies a runtime string, leaked once when the ex-command spec
/// crosses. Bounded by the loaded-source count — the `boundary_picker::intern`
/// rationale (unbounded re-registration is the hot-reload quarantine, PH7.12).
fn intern(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

impl WitBoundary for NativeLatencyClass {
    type Wit = WitLatencyClass;

    fn to_wit(&self) -> Result<WitLatencyClass, String> {
        Ok(match self {
            NativeLatencyClass::Reflex => WitLatencyClass::Reflex,
            NativeLatencyClass::Display => WitLatencyClass::Display,
            NativeLatencyClass::Background => WitLatencyClass::Background,
        })
    }

    fn from_wit(wit: WitLatencyClass) -> Result<Self, String> {
        Ok(match wit {
            WitLatencyClass::Reflex => NativeLatencyClass::Reflex,
            WitLatencyClass::Display => NativeLatencyClass::Display,
            WitLatencyClass::Background => NativeLatencyClass::Background,
        })
    }
}

impl WitBoundary for NativeSurfaceForm {
    type Wit = WitSurfaceForm;

    fn to_wit(&self) -> Result<WitSurfaceForm, String> {
        Ok(match self {
            NativeSurfaceForm::Keyword => WitSurfaceForm::Keyword,
            NativeSurfaceForm::Delimiter { hint } => WitSurfaceForm::Delimiter(hint.to_string()),
        })
    }

    fn from_wit(wit: WitSurfaceForm) -> Result<Self, String> {
        Ok(match wit {
            WitSurfaceForm::Keyword => NativeSurfaceForm::Keyword,
            WitSurfaceForm::Delimiter(hint) => NativeSurfaceForm::Delimiter { hint: intern(hint) },
        })
    }
}

impl WitBoundary for NativeMotionResult {
    type Wit = WitMotionResult;

    fn to_wit(&self) -> Result<WitMotionResult, String> {
        Ok(WitMotionResult {
            target: self.target.to_wit()?,
            linewise: self.linewise,
        })
    }

    fn from_wit(wit: WitMotionResult) -> Result<Self, String> {
        Ok(NativeMotionResult {
            target: lattice_protocol::position::Position::from_wit(wit.target)?,
            linewise: wit.linewise,
        })
    }
}

/// Project a live [`MotionContext`](NativeMotionContext) into its owned WIT
/// mirror (host→guest). Reads only the owned scalars — `&Buffer`,
/// `&CancellationToken`, and the tree-sitter `scope_resolver` are host-owned and
/// reached (if at all) through the `document` handle, never this record.
pub fn project_motion_context(ctx: &NativeMotionContext) -> Result<WitMotionContext, String> {
    Ok(WitMotionContext {
        buffer_id: ctx.buffer_id.0,
        from: ctx.from.to_wit()?,
        count: ctx.count.get(),
        has_explicit_count: ctx.has_explicit_count,
        args: ctx.args.to_wit()?,
    })
}

/// Project an [`OperatorContext`](NativeOperatorContext) (host→guest). The
/// `&mut Document` is not projected — mutation is the returned `effect` (§4.5).
pub fn project_operator_context(ctx: &NativeOperatorContext) -> Result<WitOperatorContext, String> {
    Ok(WitOperatorContext {
        range: ctx.range.to_wit()?,
        linewise: ctx.linewise,
        register: ctx.register.to_wit()?,
        count: ctx.count.get(),
        args: ctx.args.to_wit()?,
    })
}

/// Project a [`TextObjectContext`](NativeTextObjectContext) (host→guest). The
/// scope/comment env rides the `document` handle, not this record.
pub fn project_text_object_context(
    ctx: &NativeTextObjectContext,
) -> Result<WitTextObjectContext, String> {
    Ok(WitTextObjectContext {
        at: ctx.at.to_wit()?,
        count: ctx.count.get(),
        args: ctx.args.to_wit()?,
    })
}

/// Project an [`ExCommandContext`](NativeExCommandContext) (host→guest). The
/// native `range: Option<grammar::Range>` is absent — the recursive grammar
/// `Range` cannot cross a WIT record (the `Global` / `NarrowTrigger` precedent),
/// so a v1 ex-command plugin gets `bang` / `args` / `register` / `count`.
pub fn project_ex_command_context(
    ctx: &NativeExCommandContext,
) -> Result<WitExCommandContext, String> {
    Ok(WitExCommandContext {
        bang: ctx.bang,
        args: ctx.args.to_wit()?,
        register: ctx.register.to_wit()?,
        count: ctx.count.get(),
    })
}

/// Project an [`ActionContext`](NativeActionContext) (host→guest).
pub fn project_action_context(ctx: &NativeActionContext) -> Result<WitActionContext, String> {
    Ok(WitActionContext {
        args: ctx.args.to_wit()?,
        register: ctx.register.to_wit()?,
        count: ctx.count.get(),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::lattice::plugin_host::types::{Args as WitArgs, Register as WitRegister};
    use lattice_core::Document;
    use lattice_core::buffer::Buffer;
    use lattice_core::buffers::BufferId;
    use lattice_grammar::CancellationToken;
    use lattice_grammar::args::{ArgValue, Args};
    use lattice_grammar::command::Count;
    use lattice_grammar::register::Register;
    use lattice_protocol::position::{Position, Range};

    fn pos(line: u32, byte: u32) -> Position {
        Position { line, byte }
    }

    #[test]
    fn latency_class_round_trips_every_arm() {
        for native in [
            NativeLatencyClass::Reflex,
            NativeLatencyClass::Display,
            NativeLatencyClass::Background,
        ] {
            let back = NativeLatencyClass::from_wit(native.to_wit().unwrap()).unwrap();
            assert_eq!(native, back);
        }
    }

    #[test]
    fn surface_form_round_trips_both_arms() {
        assert_eq!(
            NativeSurfaceForm::from_wit(NativeSurfaceForm::Keyword.to_wit().unwrap()).unwrap(),
            NativeSurfaceForm::Keyword
        );
        let delim = NativeSurfaceForm::Delimiter {
            hint: ":s/pat/repl/",
        };
        let back = NativeSurfaceForm::from_wit(delim.to_wit().unwrap()).unwrap();
        assert_eq!(back, delim);
    }

    #[test]
    fn motion_result_round_trips() {
        let native = NativeMotionResult {
            target: pos(3, 7),
            linewise: true,
        };
        let back = NativeMotionResult::from_wit(native.to_wit().unwrap()).unwrap();
        assert_eq!(back.target, native.target);
        assert_eq!(back.linewise, native.linewise);
    }

    #[test]
    fn motion_context_projects_owned_scalars() {
        let buffer = Buffer::from_text("hello world\nsecond line\n");
        let cancel = CancellationToken::never();
        let ctx = NativeMotionContext {
            buffer: &buffer,
            buffer_id: BufferId(9),
            from: pos(1, 2),
            count: Count(4),
            has_explicit_count: true,
            args: Args::String("w".into()),
            cancel: &cancel,
            scope_resolver: None,
        };
        let wit = project_motion_context(&ctx).unwrap();
        assert_eq!(wit.buffer_id, 9);
        assert_eq!(wit.from.line, 1);
        assert_eq!(wit.from.byte, 2);
        assert_eq!(wit.count, 4);
        assert!(wit.has_explicit_count);
        assert!(matches!(wit.args, WitArgs::String(ref s) if s == "w"));
    }

    #[test]
    fn operator_context_projects_range_and_register() {
        let mut document = Document::from_text("abc\n");
        let ctx = NativeOperatorContext {
            document: &mut document,
            range: Range {
                start: pos(0, 0),
                end: pos(0, 3),
            },
            linewise: false,
            register: Register::Named('a'),
            count: Count(1),
            args: Args::None,
            cancel: &CancellationToken::never(),
        };
        let wit = project_operator_context(&ctx).unwrap();
        assert_eq!(wit.range.start.byte, 0);
        assert_eq!(wit.range.end.byte, 3);
        assert!(!wit.linewise);
        assert!(matches!(wit.register, WitRegister::Named('a')));
        assert_eq!(wit.count, 1);
    }

    #[test]
    fn text_object_context_projects_cursor_and_count() {
        let buffer = Buffer::from_text("word\n");
        let cancel = CancellationToken::never();
        let ctx = NativeTextObjectContext {
            buffer: &buffer,
            at: pos(0, 2),
            count: Count(2),
            args: Args::None,
            cancel: &cancel,
            scope_resolver: None,
            comment_syntax: None,
        };
        let wit = project_text_object_context(&ctx).unwrap();
        assert_eq!(wit.at.byte, 2);
        assert_eq!(wit.count, 2);
    }

    #[test]
    fn ex_command_context_projects_bang_and_args() {
        let ctx = NativeExCommandContext {
            bang: true,
            args: Args::List(vec![ArgValue::String("x".into()), ArgValue::Int(1)]),
            range: None,
            register: Register::Unnamed,
            count: Count(1),
            cancel: CancellationToken::never(),
        };
        let wit = project_ex_command_context(&ctx).unwrap();
        assert!(wit.bang);
        assert!(matches!(wit.register, WitRegister::Unnamed));
        assert!(matches!(wit.args, WitArgs::List(ref v) if v.len() == 2));
    }

    #[test]
    fn action_context_projects_register_and_count() {
        let ctx = NativeActionContext {
            args: Args::None,
            register: Register::System,
            count: Count(3),
            cancel: CancellationToken::never(),
        };
        let wit = project_action_context(&ctx).unwrap();
        assert_eq!(wit.count, 3);
        assert!(matches!(wit.register, WitRegister::System));
    }
}
