//! CM.1: the streamed compilation-output event.
//!
//! `CompilationService` publishes one [`CompilationOutputPushed`]
//! per streamed chunk on the editor's typed event bus;
//! `CompilationMode`'s drain task (subscribed in `on_activate`)
//! applies each chunk to the `*compilation*` buffer. Peer
//! subscribers (renderer wake, future telemetry) attach to the
//! same event with no privileged path — the codified `*messages*`
//! / ai-log streaming shape.

/// One unit of streamed compilation output. The drain applies
/// these in arrival order:
///
/// - [`OutputChunk::Reset`] clears the buffer and seeds a fresh
///   run header (full-range replace).
/// - [`OutputChunk::Append`] appends captured stdout/stderr text
///   at the end of the buffer (insert-at-end).
/// - [`OutputChunk::Finished`] appends the terminal summary line
///   once the process exits (insert-at-end).
#[derive(Debug, Clone)]
pub enum OutputChunk {
    /// Clear the buffer and seed it with the run header.
    Reset { header: String },
    /// Append a batch of captured output lines.
    Append {
        /// The captured text with every escape sequence already
        /// removed (CM.5) — this is what lands in the buffer and what
        /// the error parsers were fed.
        text: String,
        /// CM.5: one entry per line of `text`, in order, holding that
        /// line's ANSI colour spans at byte offsets into the **clean**
        /// line. Empty when the output carried no colour (the common
        /// case: a pipe makes most tools disable it) or when no theme
        /// registry was available to intern the palette.
        ///
        /// Travels with the text rather than on a parallel channel for
        /// the reason the highlight drain states about diff signs:
        /// one thing being spliced cannot desynchronise from itself.
        spans: Vec<Vec<lattice_cells::StyledSpan>>,
    },
    /// Append the exit summary line.
    Finished { summary: String },
}

impl OutputChunk {
    /// An `Append` carrying no colour — the shape most call sites and
    /// every test that does not care about ANSI wants.
    pub fn append(text: impl Into<String>) -> Self {
        OutputChunk::Append {
            text: text.into(),
            spans: Vec::new(),
        }
    }
}

/// Typed event carrying one [`OutputChunk`] from the compilation
/// service to the `*compilation*` buffer drain.
#[derive(Debug, Clone)]
pub struct CompilationOutputPushed {
    pub chunk: OutputChunk,
}

lattice_protocol::register_event!(
    CompilationOutputPushed,
    "compilation.output-pushed",
    "Fired for each chunk of streamed compilation output (run header \
     reset, appended stdout/stderr lines, exit summary). The \
     `*compilation*` buffer's drain is the primary subscriber; the \
     renderer wake and future telemetry hooks are peers.",
    "lattice-compilation",
);
