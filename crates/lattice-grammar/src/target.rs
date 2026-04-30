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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Target {
    Motion(MotionId, Args),
    TextObject(TextObjectId, Args),
    Range(Range),
}
