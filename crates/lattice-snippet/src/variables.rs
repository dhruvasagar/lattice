//! Variable resolution for snippet expansion. The host fills
//! in a [`VariableContext`] with file / cursor / clipboard
//! info; the renderer asks the context for each `$VAR` it
//! encounters.
//!
//! v1 supports the TextMate / VS Code built-in variable set:
//!
//! | Variable | Resolves to |
//! |---|---|
//! | `TM_SELECTED_TEXT` | last visual selection |
//! | `TM_CURRENT_LINE` | current line text |
//! | `TM_CURRENT_WORD` | word under cursor |
//! | `TM_LINE_INDEX` | 0-based current line |
//! | `TM_LINE_NUMBER` | 1-based current line |
//! | `TM_FILENAME` | current filename (basename + ext) |
//! | `TM_FILENAME_BASE` | filename without extension |
//! | `TM_DIRECTORY` | containing directory absolute path |
//! | `TM_FILEPATH` | absolute path to current file |
//! | `WORKSPACE_NAME` | workspace folder basename |
//! | `WORKSPACE_FOLDER` | workspace folder absolute path |
//! | `CLIPBOARD` | current `"+` register text |
//! | `CURRENT_YEAR` / `CURRENT_MONTH` / `CURRENT_DATE` / `CURRENT_HOUR` / `CURRENT_MINUTE` / `CURRENT_SECOND` | timestamp components (zero-padded where conventional) |
//! | `CURRENT_DAY_NAME` / `CURRENT_DAY_NAME_SHORT` / `CURRENT_MONTH_NAME` / `CURRENT_MONTH_NAME_SHORT` | named timestamp parts |
//! | `RANDOM` | 6-digit random int |
//! | `RANDOM_HEX` | 6-char random hex |
//! | `UUID` | random UUID v4 |
//! | `LINE_COMMENT` / `BLOCK_COMMENT_START` / `BLOCK_COMMENT_END` | per-major-mode comment strings (Phase 8) |
//!
//! Unknown variables resolve to `None` (the renderer emits the
//! variable's `default` body or the empty string).

use std::collections::HashMap;

/// Editor-supplied context the renderer consults to resolve
/// variable references. The host fills in the file / cursor
/// fields; timestamp + random / UUID values are computed at
/// expansion time inside [`VariableContext::resolve`].
///
/// `extra` is the plugin / user-defined variable slot --
/// values the host wants to expose without subclassing.
/// Names lookup falls through to `extra` when the built-in
/// set doesn't recognise the name.
#[derive(Debug, Clone, Default)]
pub struct VariableContext {
    pub selected_text: Option<String>,
    pub current_line: Option<String>,
    pub current_word: Option<String>,
    /// 0-based.
    pub line_index: Option<u32>,
    pub filename: Option<String>,
    pub directory: Option<String>,
    pub filepath: Option<String>,
    pub workspace_name: Option<String>,
    pub workspace_folder: Option<String>,
    pub clipboard: Option<String>,
    pub line_comment: Option<String>,
    pub block_comment_start: Option<String>,
    pub block_comment_end: Option<String>,
    /// User / plugin extras. Looked up after the built-in set.
    pub extra: HashMap<String, String>,
    /// Optional fixed timestamp (for tests). When None, the
    /// renderer reads `SystemTime::now()`.
    pub fixed_now: Option<TimestampParts>,
    /// Optional fixed random seed (for tests).
    pub fixed_random: Option<u64>,
}

/// Decomposed timestamp -- supplied by the host (typically
/// derived from `chrono` / `time` if it's already a dep) or
/// computed at variable-resolve time. Tests use `fixed_now` to
/// pin output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampParts {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub day_name: &'static str,
    pub day_name_short: &'static str,
    pub month_name: &'static str,
    pub month_name_short: &'static str,
}

impl VariableContext {
    /// Resolve a variable name to its expansion text. Returns
    /// `None` when neither the built-in set nor `extra` knows
    /// the name -- the renderer then emits the snippet's
    /// fallback (default body) or empty string.
    pub fn resolve(&self, name: &str) -> Option<String> {
        match name {
            "TM_SELECTED_TEXT" => self.selected_text.clone(),
            "TM_CURRENT_LINE" => self.current_line.clone(),
            "TM_CURRENT_WORD" => self.current_word.clone(),
            "TM_LINE_INDEX" => self.line_index.map(|n| n.to_string()),
            "TM_LINE_NUMBER" => self.line_index.map(|n| (n + 1).to_string()),
            "TM_FILENAME" => self.filename.clone(),
            "TM_FILENAME_BASE" => self.filename.as_ref().map(|f| {
                f.rsplit_once('.').map(|(s, _)| s.to_string()).unwrap_or(f.clone())
            }),
            "TM_DIRECTORY" => self.directory.clone(),
            "TM_FILEPATH" => self.filepath.clone(),
            "WORKSPACE_NAME" => self.workspace_name.clone(),
            "WORKSPACE_FOLDER" => self.workspace_folder.clone(),
            "CLIPBOARD" => self.clipboard.clone(),
            "LINE_COMMENT" => self.line_comment.clone(),
            "BLOCK_COMMENT_START" => self.block_comment_start.clone(),
            "BLOCK_COMMENT_END" => self.block_comment_end.clone(),
            // Timestamps -- `fixed_now` for tests; otherwise
            // SystemTime-derived. Date / Day equal the current
            // day of month; the month's name is supplied in
            // TimestampParts.
            "CURRENT_YEAR" => Some(self.now().year.to_string()),
            "CURRENT_YEAR_SHORT" => Some(format!("{:02}", self.now().year % 100)),
            "CURRENT_MONTH" => Some(format!("{:02}", self.now().month)),
            "CURRENT_DATE" => Some(format!("{:02}", self.now().day)),
            "CURRENT_HOUR" => Some(format!("{:02}", self.now().hour)),
            "CURRENT_MINUTE" => Some(format!("{:02}", self.now().minute)),
            "CURRENT_SECOND" => Some(format!("{:02}", self.now().second)),
            "CURRENT_DAY_NAME" => Some(self.now().day_name.to_string()),
            "CURRENT_DAY_NAME_SHORT" => Some(self.now().day_name_short.to_string()),
            "CURRENT_MONTH_NAME" => Some(self.now().month_name.to_string()),
            "CURRENT_MONTH_NAME_SHORT" => {
                Some(self.now().month_name_short.to_string())
            }
            "RANDOM" => Some(format!("{:06}", self.random() % 1_000_000)),
            "RANDOM_HEX" => Some(format!("{:06x}", self.random() & 0xff_ffff)),
            "UUID" => Some(uuid_v4_str(self.random())),
            other => self.extra.get(other).cloned(),
        }
    }

    fn now(&self) -> TimestampParts {
        if let Some(fx) = self.fixed_now.as_ref() {
            return fx.clone();
        }
        // Real-clock path -- compute from SystemTime. Falls
        // back to a fixed default on UNIX-time errors (clock
        // before epoch -- shouldn't happen).
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        compute_timestamp_parts(now)
    }

    fn random(&self) -> u64 {
        if let Some(seed) = self.fixed_random {
            return seed;
        }
        // Cheap pseudo-random from clock ns -- snippets don't
        // need cryptographic randomness. Avoids pulling in
        // `rand` as a dep just for this.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0);
        // xorshift step.
        let mut x = nanos.wrapping_add(0x9e37_79b9_7f4a_7c15);
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    }
}

/// Convenience: `VariableContext` populated with the builtin
/// timestamp + random sources but no editor-supplied fields.
/// Useful for "expand this snippet outside an editor" flows
/// (tests, examples, snippet authoring tools).
pub fn builtin_variables() -> VariableContext {
    VariableContext::default()
}

fn compute_timestamp_parts(unix_secs: u64) -> TimestampParts {
    // Approximate Gregorian date math. Good enough for snippet
    // variables -- avoids the `chrono` dependency. Day-of-week
    // computation uses Zeller's congruence. Month names mirror
    // VS Code's variable values.
    let days = unix_secs / 86_400;
    let secs = unix_secs % 86_400;
    let hour = (secs / 3600) as u32;
    let minute = ((secs % 3600) / 60) as u32;
    let second = (secs % 60) as u32;

    let (year, month, day) = days_to_ymd(days);
    let day_of_week = day_of_week_zeller(year, month, day);
    let day_name = match day_of_week {
        0 => "Saturday",
        1 => "Sunday",
        2 => "Monday",
        3 => "Tuesday",
        4 => "Wednesday",
        5 => "Thursday",
        6 => "Friday",
        _ => "Sunday",
    };
    let day_name_short = match day_name {
        "Saturday" => "Sat",
        "Sunday" => "Sun",
        "Monday" => "Mon",
        "Tuesday" => "Tue",
        "Wednesday" => "Wed",
        "Thursday" => "Thu",
        "Friday" => "Fri",
        _ => "Sun",
    };
    let month_name = match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => "Unknown",
    };
    let month_name_short = match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "Unk",
    };
    TimestampParts {
        year,
        month,
        day,
        hour,
        minute,
        second,
        day_name,
        day_name_short,
        month_name,
        month_name_short,
    }
}

fn days_to_ymd(mut days: u64) -> (u32, u32, u32) {
    // Days since 1970-01-01.
    let mut year: u64 = 1970;
    loop {
        let year_days = if is_leap_year(year as u32) { 366 } else { 365 };
        if days < year_days {
            break;
        }
        days -= year_days;
        year += 1;
    }
    let month_days = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut month: u32 = 1;
    for (i, m_days) in month_days.iter().enumerate() {
        let m_days = *m_days
            + if i == 1 && is_leap_year(year as u32) {
                1
            } else {
                0
            };
        if days < m_days {
            month = i as u32 + 1;
            break;
        }
        days -= m_days;
    }
    let day = days as u32 + 1;
    (year as u32, month, day)
}

fn is_leap_year(y: u32) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

fn day_of_week_zeller(year: u32, month: u32, day: u32) -> u32 {
    // Zeller's congruence (Gregorian). Returns 0=Saturday,
    // 1=Sunday, ..., 6=Friday.
    let (m, y) = if month < 3 {
        (month + 12, year - 1)
    } else {
        (month, year)
    };
    let k = y % 100;
    let j = y / 100;
    let h = (day + (13 * (m + 1)) / 5 + k + k / 4 + j / 4 + 5 * j) % 7;
    h
}

/// Cheap UUID v4 string from a 64-bit seed. Not RFC-cryptographic,
/// just structurally valid. Snippet `$UUID` doesn't need
/// security guarantees.
fn uuid_v4_str(seed: u64) -> String {
    let mut a = seed;
    let mut b = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    a ^= a << 13;
    a ^= a >> 7;
    a ^= a << 17;
    b ^= b << 13;
    b ^= b >> 7;
    b ^= b << 17;
    let lo = a;
    let hi = b;
    // Layout: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx (y = 8|9|a|b).
    let p1 = (lo >> 32) as u32;
    let p2 = (lo & 0xffff_ffff) as u16;
    let mut p3 = ((hi >> 48) & 0x0fff) as u16;
    p3 |= 0x4000;
    let mut p4 = ((hi >> 32) & 0x3fff) as u16;
    p4 |= 0x8000;
    let p5 = hi & 0xffff_ffff_ffff;
    format!(
        "{p1:08x}-{p2:04x}-{p3:04x}-{p4:04x}-{p5:012x}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tm_filename_resolves() {
        let mut ctx = VariableContext::default();
        ctx.filename = Some("foo.rs".into());
        assert_eq!(ctx.resolve("TM_FILENAME"), Some("foo.rs".into()));
        assert_eq!(ctx.resolve("TM_FILENAME_BASE"), Some("foo".into()));
    }

    #[test]
    fn tm_line_number_is_one_based() {
        let mut ctx = VariableContext::default();
        ctx.line_index = Some(42);
        assert_eq!(ctx.resolve("TM_LINE_INDEX"), Some("42".into()));
        assert_eq!(ctx.resolve("TM_LINE_NUMBER"), Some("43".into()));
    }

    #[test]
    fn unknown_variable_falls_through_to_extras() {
        let mut ctx = VariableContext::default();
        ctx.extra.insert("MY_VAR".into(), "value".into());
        assert_eq!(ctx.resolve("MY_VAR"), Some("value".into()));
        assert_eq!(ctx.resolve("OTHER"), None);
    }

    #[test]
    fn fixed_now_pins_timestamp_variables() {
        let mut ctx = VariableContext::default();
        ctx.fixed_now = Some(TimestampParts {
            year: 2026,
            month: 5,
            day: 6,
            hour: 14,
            minute: 30,
            second: 0,
            day_name: "Tuesday",
            day_name_short: "Tue",
            month_name: "May",
            month_name_short: "May",
        });
        assert_eq!(ctx.resolve("CURRENT_YEAR"), Some("2026".into()));
        assert_eq!(ctx.resolve("CURRENT_MONTH"), Some("05".into()));
        assert_eq!(ctx.resolve("CURRENT_DATE"), Some("06".into()));
        assert_eq!(ctx.resolve("CURRENT_HOUR"), Some("14".into()));
        assert_eq!(ctx.resolve("CURRENT_DAY_NAME"), Some("Tuesday".into()));
        assert_eq!(ctx.resolve("CURRENT_MONTH_NAME"), Some("May".into()));
    }

    #[test]
    fn fixed_random_pins_random_variables() {
        let mut ctx = VariableContext::default();
        ctx.fixed_random = Some(123_456_789);
        // Modulo 1_000_000 then zero-padded to 6 digits.
        assert_eq!(ctx.resolve("RANDOM"), Some("456789".into()));
        // Hex variant -- low 6 hex digits.
        assert!(ctx.resolve("RANDOM_HEX").unwrap().chars().all(|c| {
            c.is_ascii_hexdigit()
        }));
    }

    #[test]
    fn uuid_resolves_to_36_char_dashed_string() {
        let mut ctx = VariableContext::default();
        ctx.fixed_random = Some(0x12345678);
        let id = ctx.resolve("UUID").unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(id.matches('-').count(), 4);
        // Version 4 marker at the right offset.
        assert_eq!(id.chars().nth(14), Some('4'));
    }

    #[test]
    fn days_to_ymd_round_trips_known_dates() {
        // 2026-01-01 = 20454 days since 1970-01-01.
        let secs = 20454u64 * 86_400;
        let parts = compute_timestamp_parts(secs);
        assert_eq!(parts.year, 2026);
        assert_eq!(parts.month, 1);
        assert_eq!(parts.day, 1);
    }

    #[test]
    fn leap_year_february_handles_feb_29() {
        // 2024 is a leap year. 2024-02-29 = 19782 days since
        // epoch.
        let secs = 19782u64 * 86_400;
        let parts = compute_timestamp_parts(secs);
        assert_eq!(parts.year, 2024);
        assert_eq!(parts.month, 2);
        assert_eq!(parts.day, 29);
    }
}
