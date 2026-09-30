//! `Target`: the value an operator operates on.
//!
//! A `Target` resolves (via the dispatcher) to a structural buffer range. It
//! can be a motion (consumed by following the motion's evaluator from the
//! current cursor), a text-object (consumed by evaluating the text-object at
//! the current cursor), or an explicit grammar `Range` (line range, mark
//! range, `:%`, current selection, etc.).

use serde::{Deserialize, Serialize};

use crate::args::Args;
use crate::range::Range;
use crate::registry::{MotionId, TextObjectId};

/// What an operator acts on: the `w` in `dw`, the `iw` in `diw`, the `%` in
/// `:%d`. The dispatcher resolves it against the document and cursor to a
/// concrete span before calling the operator.
///
/// An invocation's explicit `range` wins over its `target` when both are
/// set; an operator with neither fails with
/// [`crate::CommandError::MissingTarget`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Target {
    /// Run the motion from the cursor; the span is cursor..landing point,
    /// with the motion's own inclusive / linewise flags deciding the ends.
    /// The [`Args`] are the motion's own (e.g. the char for `f<c>`). If the
    /// motion cannot move, the operator is cancelled
    /// ([`crate::CommandError::MotionFailed`]).
    Motion(MotionId, Args),
    /// Evaluate the text object at the cursor (`iw`, `a(`, tree-sitter
    /// `af`); the span is whatever it selects. [`Args`] as for `Motion`.
    TextObject(TextObjectId, Args),
    /// An explicit grammar [`Range`] -- line range, marks, `%`, or the
    /// current Visual selection.
    Range(Range),
}
