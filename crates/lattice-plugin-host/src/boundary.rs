//! The boundary adapter machinery (plugin-host.md §4).
//!
//! Design fragment §4. Slice: PH7.3a (conventions + `Args`); the `Effect`
//! variant mirror is PH7.3b; the `document` resource handle + owned-snapshot
//! projection is PH7.3c.
//!
//! Native host types (`lattice_grammar::args::Args`, `Effect`, the picker /
//! completion records) cannot cross a WASM boundary as-is — they carry
//! borrows, closures, `Future`/`Stream` carriers, or non-serde fields (§4.1–
//! §4.5). The **generated WIT mirror** (from `wit/types.wit`, emitted by
//! `bindgen!` at the crate root) is the owned, serializable shape the guest
//! sees. [`WitBoundary`] is the single adapter contract between the two: one
//! place that defines `to_wit` / `from_wit` and the `Result<_, String>` error
//! convention, so every type round-trips uniformly and a malformed payload is
//! rejected at the boundary rather than at apply-time.
//!
//! The `String` error arm is deliberate: it is the WIT `result<_, string>`
//! convention (§4, "Result<_, string> error convention"), so a conversion that
//! cannot be represented yet (e.g. a nested `CommandInvocation`, §4.1) surfaces
//! as a typed error the host logs + skips, never a panic or a lossy encoding.

// The generated WIT mirrors live at the crate root (the `bindgen!` site);
// alias them so native/`Wit` pairs read unambiguously.
use crate::{ArgValue as WitArgValue, Args as WitArgs};
use lattice_grammar::args::{ArgValue as NativeArgValue, Args as NativeArgs};

/// Converts a native host type to/from its generated WIT mirror.
///
/// `to_wit` borrows the native value (the host owns it); `from_wit` consumes
/// the WIT value (it arrived by value across the boundary). Both return
/// `Result<_, String>` — the WIT `result<_, string>` convention — so a value
/// that cannot be represented in the current WIT surface is a typed error, not
/// a panic or a silent drop.
pub trait WitBoundary: Sized {
    /// The generated WIT mirror type.
    type Wit;

    /// Project the native value into its owned WIT mirror.
    fn to_wit(&self) -> Result<Self::Wit, String>;

    /// Reconstruct the native value from its WIT mirror.
    fn from_wit(wit: Self::Wit) -> Result<Self, String>;
}

impl WitBoundary for NativeArgValue {
    type Wit = WitArgValue;

    fn to_wit(&self) -> Result<WitArgValue, String> {
        Ok(match self {
            NativeArgValue::String(s) => WitArgValue::String(s.clone()),
            NativeArgValue::Char(c) => WitArgValue::Char(*c),
            NativeArgValue::Bool(b) => WitArgValue::Bool(*b),
            NativeArgValue::Int(i) => WitArgValue::Int(*i),
            NativeArgValue::Pattern(s) => WitArgValue::Pattern(s.clone()),
            NativeArgValue::Chord(s) => WitArgValue::Chord(s.clone()),
            NativeArgValue::Raw(s) => WitArgValue::Raw(s.clone()),
            // A nested invocation needs the command mirror (§4.1); until then
            // it crosses as a typed error rather than a lossy string.
            NativeArgValue::Invocation(_) => {
                return Err(
                    "ArgValue::Invocation crosses the boundary with the command mirror \
                     (PH7.3b, fragment §4.1)"
                        .to_string(),
                );
            }
        })
    }

    fn from_wit(wit: WitArgValue) -> Result<Self, String> {
        Ok(match wit {
            WitArgValue::String(s) => NativeArgValue::String(s),
            WitArgValue::Char(c) => NativeArgValue::Char(c),
            WitArgValue::Bool(b) => NativeArgValue::Bool(b),
            WitArgValue::Int(i) => NativeArgValue::Int(i),
            WitArgValue::Pattern(s) => NativeArgValue::Pattern(s),
            WitArgValue::Chord(s) => NativeArgValue::Chord(s),
            WitArgValue::Raw(s) => NativeArgValue::Raw(s),
        })
    }
}

impl WitBoundary for NativeArgs {
    type Wit = WitArgs;

    fn to_wit(&self) -> Result<WitArgs, String> {
        Ok(match self {
            NativeArgs::None => WitArgs::None,
            NativeArgs::Char(c) => WitArgs::Char(*c),
            NativeArgs::String(s) => WitArgs::String(s.clone()),
            NativeArgs::Bytes(b) => WitArgs::Bytes(b.clone()),
            NativeArgs::List(values) => WitArgs::List(
                values
                    .iter()
                    .map(WitBoundary::to_wit)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        })
    }

    fn from_wit(wit: WitArgs) -> Result<Self, String> {
        Ok(match wit {
            WitArgs::None => NativeArgs::None,
            WitArgs::Char(c) => NativeArgs::Char(c),
            WitArgs::String(s) => NativeArgs::String(s),
            WitArgs::Bytes(b) => NativeArgs::Bytes(b),
            WitArgs::List(values) => NativeArgs::List(
                values
                    .into_iter()
                    .map(NativeArgValue::from_wit)
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;

    /// native → WIT → native is the identity for every representable value.
    fn assert_args_round_trips(native: NativeArgs) {
        let wit = native.to_wit().expect("to_wit");
        let back = NativeArgs::from_wit(wit).expect("from_wit");
        assert_eq!(native, back);
    }

    #[test]
    fn args_round_trip_covers_every_representable_variant() {
        assert_args_round_trips(NativeArgs::None);
        assert_args_round_trips(NativeArgs::Char('x'));
        assert_args_round_trips(NativeArgs::String("hello".into()));
        assert_args_round_trips(NativeArgs::Bytes(vec![0, 1, 2, 255]));
        assert_args_round_trips(NativeArgs::List(vec![
            NativeArgValue::String("s".into()),
            NativeArgValue::Char('c'),
            NativeArgValue::Bool(true),
            NativeArgValue::Int(-42),
            NativeArgValue::Pattern("re".into()),
            NativeArgValue::Chord("<C-c>".into()),
            NativeArgValue::Raw("body".into()),
        ]));
    }

    #[test]
    fn arg_value_int_survives_full_i64_range() {
        for i in [i64::MIN, -1, 0, 1, i64::MAX] {
            let v = NativeArgValue::Int(i);
            let back = NativeArgValue::from_wit(v.to_wit().unwrap()).unwrap();
            assert_eq!(v, back);
        }
    }

    #[test]
    fn nested_invocation_is_a_typed_error_not_a_panic() {
        use lattice_grammar::CommandId;
        use lattice_grammar::command::CommandInvocation;
        // A nested invocation cannot cross until the command mirror lands; it
        // must surface as a typed Err, and a containing Args must propagate it.
        let invocation = CommandInvocation::of(CommandId::new(0));
        let native = NativeArgs::List(vec![NativeArgValue::Invocation(Box::new(invocation))]);
        let err = native
            .to_wit()
            .expect_err("nested invocation must not cross yet");
        assert!(err.contains("Invocation"), "error names the culprit: {err}");
    }
}
