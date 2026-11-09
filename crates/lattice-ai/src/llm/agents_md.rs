//! `AGENTS.md`: a project's instructions for AI tools, read into the system
//! prompt (design §6 "Safety lists").
//!
//! The format's own rule picks the file: the nearest `AGENTS.md` between the
//! file being worked on and the project root. It joins the system prompt after
//! the configured one, capped at [`MAX_BYTES`], and is held to `llm.exclude`
//! like any other text sent. It is trusted as a project's `system` prompt is:
//! it instructs the model and can turn nothing on. `llm.agents_md = false`
//! leaves it out.
//!
//! Blocking file reads: call these off the editor thread.

use std::io::Read;
use std::path::{Path, PathBuf};

pub const FILE_NAME: &str = "AGENTS.md";

/// The most of the file sent. Instructions past it are left out, and said so.
pub const MAX_BYTES: usize = 32 * 1024;

/// The nearest `AGENTS.md` from `start` (a file or a directory) up to `root`,
/// inclusive. A `start` outside `root` looks in `root` alone: the walk never
/// leaves the project.
pub fn find(start: Option<&Path>, root: &Path) -> Option<PathBuf> {
    let from = start
        .filter(|s| s.starts_with(root))
        .map(|s| {
            if s.is_dir() {
                s.to_path_buf()
            } else {
                s.parent()
                    .map_or_else(|| root.to_path_buf(), Path::to_path_buf)
            }
        })
        .unwrap_or_else(|| root.to_path_buf());
    from.ancestors()
        .take_while(|dir| dir.starts_with(root))
        .map(|dir| dir.join(FILE_NAME))
        .find(|candidate| candidate.is_file())
}

/// What was read: the text, and whether it was cut at [`MAX_BYTES`].
#[derive(Debug, PartialEq, Eq)]
pub struct Instructions {
    pub text: String,
    pub cut: bool,
}

/// Read `path`, at most [`MAX_BYTES`] of it, cut back to a character boundary.
pub fn read(path: &Path) -> std::io::Result<Instructions> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    let cut = bytes.len() > MAX_BYTES;
    bytes.truncate(MAX_BYTES);
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(e) => {
            // Cut inside a character: keep what is whole.
            let valid = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).unwrap_or_default()
        }
    };
    Ok(Instructions { text, cut })
}

/// The system prompt with the instructions joined after it. `shown` names the
/// file as the model is told it (relative to the root).
pub fn join(system: Option<String>, shown: &str, read: &Instructions) -> String {
    let mut out = system.unwrap_or_default();
    if !out.is_empty() {
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "The project's instructions for AI tools, from {shown}:\n\n{}",
        read.text.trim_end()
    ));
    if read.cut {
        out.push_str(&format!(
            "\n\n[{shown} continues past {MAX_BYTES} bytes; the rest was not sent]"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("lattice-agents-md-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_nearest_file_wins_and_the_walk_stays_in_the_project() {
        let s = Scratch::new("find");
        let root = s.0.join("project");
        std::fs::create_dir_all(root.join("crates/a/src")).unwrap();
        std::fs::write(s.0.join(FILE_NAME), "outside").unwrap();
        let file = root.join("crates/a/src/lib.rs");
        assert_eq!(find(Some(&file), &root), None, "the parent's is not read");
        std::fs::write(root.join(FILE_NAME), "root").unwrap();
        assert_eq!(find(Some(&file), &root), Some(root.join(FILE_NAME)));
        std::fs::write(root.join("crates/a").join(FILE_NAME), "crate").unwrap();
        assert_eq!(
            find(Some(&file), &root),
            Some(root.join("crates/a").join(FILE_NAME))
        );
        assert_eq!(find(None, &root), Some(root.join(FILE_NAME)));
        let elsewhere = s.0.join("notes.txt");
        assert_eq!(
            find(Some(&elsewhere), &root),
            Some(root.join(FILE_NAME)),
            "a file outside the project reads the root's"
        );
    }

    #[test]
    fn a_long_file_is_cut_on_a_character_and_says_so() {
        let s = Scratch::new("cut");
        let path = s.0.join(FILE_NAME);
        // 2- and 3-byte characters: MAX_BYTES falls inside one.
        let long = "é€".repeat(MAX_BYTES);
        std::fs::write(&path, &long).unwrap();
        let r = read(&path).unwrap();
        assert!(r.cut);
        assert!(r.text.len() <= MAX_BYTES && r.text.len() > MAX_BYTES - 4);
        assert!(long.starts_with(&r.text));
        let joined = join(Some("Be brief.".into()), FILE_NAME, &r);
        assert!(joined.starts_with("Be brief.\n\nThe project's instructions"));
        assert!(joined.ends_with("the rest was not sent]"));

        std::fs::write(&path, "Use tabs.\n").unwrap();
        let r = read(&path).unwrap();
        assert_eq!(
            join(None, "crates/a/AGENTS.md", &r),
            "The project's instructions for AI tools, from crates/a/AGENTS.md:\n\nUse tabs."
        );
    }
}
