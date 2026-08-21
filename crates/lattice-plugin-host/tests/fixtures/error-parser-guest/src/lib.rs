//! CM.6 error-parser fixture guest.
//!
//! Recognises a two-line format no native parser knows:
//!
//! ```text
//! ERR something broke
//!   at src/thing.q:12:5
//! ```
//!
//! Two lines on purpose. A single-line format would be satisfied by a regex
//! and would not exercise the thing the seam actually has to support —
//! pending state carried across `feed` calls, and `reset` dropping it.

wit_bindgen::generate!({
    world: "error-parser-plugin",
    path: "../../../../../wit",
});

// `Entry` arrives at the world root via the world's `use error-parser.{entry}`;
// importing it again from the interface would be a duplicate definition.
use lattice::plugin_host::error_parser::Severity;
use std::cell::RefCell;

thread_local! {
    /// The header seen but not yet located. This is the state the host's
    /// per-instance `Store` isolates.
    static PENDING: RefCell<Option<(Severity, String)>> = const { RefCell::new(None) };
}

struct Component;

impl Guest for Component {
    fn reset() {
        PENDING.with(|p| *p.borrow_mut() = None);
    }

    fn feed(line: String) -> Vec<Entry> {
        let trimmed = line.trim();
        // A header primes the pending diagnostic and completes nothing.
        for (prefix, severity) in [("ERR ", Severity::Error), ("WARN ", Severity::Warning)] {
            if let Some(msg) = trimmed.strip_prefix(prefix) {
                PENDING.with(|p| *p.borrow_mut() = Some((severity, msg.to_string())));
                return Vec::new();
            }
        }
        // A locator completes it.
        let Some(rest) = trimmed.strip_prefix("at ") else {
            return Vec::new();
        };
        let Some((severity, message)) = PENDING.with(|p| p.borrow_mut().take()) else {
            return Vec::new();
        };
        let parts: Vec<&str> = rest.rsplitn(3, ':').collect();
        if parts.len() != 3 {
            return Vec::new();
        }
        // rsplitn yields reversed: col, line, path.
        let (Ok(col), Ok(line_no)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>()) else {
            return Vec::new();
        };
        vec![Entry {
            path: parts[2].to_string(),
            // The tool prints 1-based; the host's contract is 0-based, and
            // doing the conversion here keeps one convention host-side.
            line: line_no.saturating_sub(1),
            col: col.saturating_sub(1),
            severity,
            message,
        }]
    }
}

export!(Component);
