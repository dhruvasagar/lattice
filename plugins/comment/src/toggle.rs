//! CM.3 — what `gc` does to a range of lines, and nothing else.
//!
//! Every function here is pure. The seam wiring — the operator registration,
//! the `document` reads, the `apply-edit` effects — lives in `lib.rs`, so the
//! toggle's semantics can be tested without a WASM host (the `projects.rs`
//! precedent in the `project` plugin).
//!
//! ## The three rules, and why they are these
//!
//! Vim's commentary, Neovim's built-in `gc`, Helix and Zed all agree, and
//! disagreeing here would be the wrong kind of original:
//!
//! 1. **Uncomment only if EVERY non-blank line is already commented.** A mixed
//!    range comments. Per-line toggling would turn a partly-commented
//!    selection inside out, which is never the intent.
//! 2. **Insert at the range's MINIMUM indent**, not column 0, so relative
//!    structure survives. Commenting indented code at column 0 is the thing
//!    users notice first and forgive least.
//! 3. **Skip blank lines.** A commented blank line is trailing whitespace with
//!    extra steps, and it would break rule 1 on the way back.

/// Line-comment leaders, keyed on file extension.
///
/// **Line-comment forms only.** CSS and Markdown are deliberately absent:
/// `/*` and `<!--` are *block* delimiters, and emitting one per line would
/// write syntax errors into the user's file. Block comments are a separate
/// feature, not a table entry.
const LEADERS: &[(&str, &str)] = &[
    ("rs", "//"),
    ("c", "//"),
    ("h", "//"),
    ("cpp", "//"),
    ("hpp", "//"),
    ("go", "//"),
    ("java", "//"),
    ("js", "//"),
    ("jsx", "//"),
    ("ts", "//"),
    ("tsx", "//"),
    ("wit", "//"),
    ("py", "#"),
    ("rb", "#"),
    ("sh", "#"),
    ("bash", "#"),
    ("yaml", "#"),
    ("yml", "#"),
    ("toml", "#"),
    ("sql", "--"),
    ("lua", "--"),
];

/// The leader for `path`'s extension, if this plugin knows one.
///
/// Extension rather than `tree-snapshot.language()`: the operator has no tree,
/// and `document` exposes `path()` but no language. It is also the more robust
/// key — it answers for a buffer whose parse has not landed, or whose language
/// has no grammar at all.
pub fn leader_for_path(path: &str) -> Option<&'static str> {
    let ext = path.rsplit_once('.').map(|(_, e)| e)?;
    LEADERS
        .iter()
        .find(|(name, _)| *name == ext)
        .map(|(_, leader)| *leader)
}

/// Leading-whitespace width of `line`, in bytes.
pub fn indent_of(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

pub fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Is `line` already commented with `leader`?
pub fn is_commented(line: &str, leader: &str) -> bool {
    line.trim_start().starts_with(leader)
}

/// Strip the first `leader`, and one following space if there is one.
///
/// The space is optional on the way out even though this plugin always writes
/// one, because the line may have been commented by something else — another
/// editor, a formatter, a human.
pub fn uncomment(line: &str, leader: &str) -> String {
    let indent = &line[..indent_of(line)];
    let rest = line.trim_start();
    let stripped = rest.strip_prefix(leader).unwrap_or(rest);
    let stripped = stripped.strip_prefix(' ').unwrap_or(stripped);
    format!("{indent}{stripped}")
}

/// Insert `leader` at `col` — the range's minimum indent.
pub fn comment(line: &str, leader: &str, col: usize, leader_space: bool) -> String {
    let col = col.min(line.len());
    let sep = if leader_space { " " } else { "" };
    format!("{}{leader}{sep}{}", &line[..col], &line[col..])
}

/// The whole decision: given the range's lines, return each line's new text.
///
/// `None` for a line means "leave it exactly as it is" — blank lines, and
/// lines the toggle would not change. The caller emits no edit for those,
/// which keeps `gc` off the undo stack when it would be a no-op.
pub fn toggle(lines: &[String], leader: &str, leader_space: bool) -> Vec<Option<String>> {
    let significant: Vec<&String> = lines.iter().filter(|l| !is_blank(l)).collect();
    if significant.is_empty() {
        return vec![None; lines.len()];
    }

    // Rule 1.
    let all_commented = significant.iter().all(|l| is_commented(l, leader));
    // Rule 2.
    let col = significant
        .iter()
        .map(|l| indent_of(l))
        .min()
        .unwrap_or(0);

    lines
        .iter()
        .map(|line| {
            // Rule 3.
            if is_blank(line) {
                return None;
            }
            let next = if all_commented {
                uncomment(line, leader)
            } else {
                comment(line, leader, col, leader_space)
            };
            if next == *line { None } else { Some(next) }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Apply a toggle, keeping unchanged lines, so a round trip can be compared
    /// against the original byte for byte.
    fn apply(src: &[&str], leader: &str) -> Vec<String> {
        let input = lines(src);
        toggle(&input, leader, true)
            .into_iter()
            .zip(input)
            .map(|(next, old)| next.unwrap_or(old))
            .collect()
    }

    #[test]
    fn comments_a_plain_line() {
        assert_eq!(apply(&["let x = 1;"], "//"), lines(&["// let x = 1;"]));
    }

    #[test]
    fn uncomments_when_every_line_is_commented() {
        assert_eq!(
            apply(&["// a", "// b"], "//"),
            lines(&["a", "b"]),
            "rule 1: all commented ⇒ uncomment"
        );
    }

    /// Rule 1's real content. Per-line toggling would produce `["a", "// b"]` —
    /// the selection inverted, which is never what anyone wants.
    #[test]
    fn a_mixed_range_comments_rather_than_inverting() {
        assert_eq!(
            apply(&["// a", "b"], "//"),
            lines(&["// // a", "// b"]),
            "rule 1: any uncommented line ⇒ comment the whole range"
        );
    }

    /// Rule 2. Column-0 insertion in indented code is the thing users notice
    /// first, and the reason is that it destroys the shape of the block.
    #[test]
    fn inserts_at_the_ranges_minimum_indent() {
        assert_eq!(
            apply(&["    if x {", "        y();", "    }"], "//"),
            lines(&["    // if x {", "    //     y();", "    // }"]),
            "rule 2: the leader lands at the shallowest indent, and the deeper \
             line keeps its extra four spaces"
        );
    }

    /// Rule 3, both halves: a blank line is not commented on the way in, and
    /// therefore does not block the uncomment on the way back.
    #[test]
    fn blank_lines_are_skipped_in_both_directions() {
        assert_eq!(
            apply(&["a", "", "b"], "//"),
            lines(&["// a", "", "// b"]),
            "no leader on the blank line"
        );
        assert_eq!(
            apply(&["// a", "", "// b"], "//"),
            lines(&["a", "", "b"]),
            "and the blank does not make the range look 'not all commented'"
        );
    }

    /// The property that matters most: the buffer is unchanged after a round
    /// trip. Anything else loses the user's text.
    #[test]
    fn a_round_trip_restores_the_buffer_byte_for_byte() {
        for src in [
            vec!["let x = 1;"],
            vec!["    indented();"],
            vec!["fn f() {", "    body();", "}"],
            vec!["a", "", "b"],
            vec!["        deep();", "  shallow();"],
            vec!["x  // trailing comment"],
        ] {
            let commented = apply(&src, "//");
            let back: Vec<String> = toggle(&commented, "//", true)
                .into_iter()
                .zip(commented.clone())
                .map(|(next, old)| next.unwrap_or(old))
                .collect();
            assert_eq!(back, lines(&src), "round trip of {src:?} via {commented:?}");
        }
    }

    /// A line already commented WITHOUT the space this plugin writes still
    /// uncomments — it may have been commented by another editor or a human.
    #[test]
    fn uncomments_a_leader_with_no_following_space() {
        assert_eq!(apply(&["//a"], "//"), lines(&["a"]));
    }

    #[test]
    fn a_range_of_only_blank_lines_changes_nothing() {
        assert_eq!(toggle(&lines(&["", "  "]), "//", true), vec![None, None]);
    }

    #[test]
    fn leader_space_off_writes_no_space() {
        assert_eq!(toggle(&lines(&["a"]), "//", false), vec![Some("//a".to_string())]);
        // And it still round-trips: `uncomment` strips the optional space, so
        // a buffer commented with the option off uncomments with it on.
        assert_eq!(uncomment("//a", "//"), "a");
    }

    #[test]
    fn hash_and_dash_leaders_work_the_same_way() {
        assert_eq!(apply(&["x = 1"], "#"), lines(&["# x = 1"]));
        assert_eq!(apply(&["SELECT 1"], "--"), lines(&["-- SELECT 1"]));
    }

    #[test]
    fn the_leader_comes_from_the_extension() {
        assert_eq!(leader_for_path("src/lib.rs"), Some("//"));
        assert_eq!(leader_for_path("a/b/main.py"), Some("#"));
        assert_eq!(leader_for_path("q.sql"), Some("--"));
        assert_eq!(leader_for_path("Makefile"), None, "no extension");
        assert_eq!(leader_for_path("notes.md"), None, "block-only, deliberately absent");
        assert_eq!(leader_for_path("style.css"), None, "block-only, deliberately absent");
    }
}
