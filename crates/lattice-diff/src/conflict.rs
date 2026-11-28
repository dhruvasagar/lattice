//! Reading git's conflict markers in a working file.
//!
//! A conflicted file already says where its conflicts are: git writes each
//! one as a block opened by `<<<<<<<` and closed by `>>>>>>>`. Where a
//! question is "where is the conflict", the markers answer it directly and
//! at once, without waiting for a diff of the file against its stages.

/// The 0-based line of the first `<<<<<<<` marker in `lines`, or `None`
/// when the file has no conflict block.
///
/// A marker is seven `<` at the start of the line, followed by a space
/// (git's `<<<<<<< HEAD`) or nothing. Eight or more is not a marker, so a
/// line of text that happens to start with angle brackets is not taken
/// for one.
pub fn first_marker_line<S: AsRef<str>>(lines: impl IntoIterator<Item = S>) -> Option<u32> {
    lines
        .into_iter()
        .position(|line| is_opening_marker(line.as_ref()))
        .and_then(|i| u32::try_from(i).ok())
}

fn is_opening_marker(line: &str) -> bool {
    let line = line.trim_end_matches(['\n', '\r']);
    match line.strip_prefix("<<<<<<<") {
        Some(rest) => rest.is_empty() || rest.starts_with(' '),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_block_is_found() {
        let text = "keep\n<<<<<<< HEAD\nours\n=======\ntheirs\n>>>>>>> other\n<<<<<<< HEAD\n";
        assert_eq!(first_marker_line(text.lines()), Some(1));
    }

    #[test]
    fn a_bare_marker_counts() {
        assert_eq!(first_marker_line(["a", "<<<<<<<", "b"]), Some(1));
    }

    #[test]
    fn a_marker_keeps_its_line_ending() {
        assert_eq!(first_marker_line(["a\n", "<<<<<<< HEAD\r\n"]), Some(1));
    }

    #[test]
    fn eight_angle_brackets_or_a_word_after_seven_are_not_markers() {
        assert_eq!(
            first_marker_line(["<<<<<<<< not one", "<<<<<<<x", "text"]),
            None
        );
    }

    #[test]
    fn a_file_without_conflicts_has_none() {
        assert_eq!(first_marker_line(["a", "b"]), None);
    }
}
