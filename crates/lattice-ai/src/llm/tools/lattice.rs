//! Lattice's own read tools (LL.16): `read_file`, `list_directory`, `search`.
//!
//! **What the user sees is what a read returns.** A file open in a buffer is
//! read from the buffer, unsaved edits and all, through
//! [`EditorAccess::read_text_file`]; a search overlays every open buffer on
//! what it walks from disk. A model reasoning over the saved file while the
//! user looks at an edited one would propose edits to text that is not there.
//!
//! **Reads stay inside the project and outside `llm.exclude`.** These tools
//! run without asking (design §6), and a tool result is untrusted input that
//! can carry instructions, so a read that could reach `~/.ssh` would let a
//! prompt injected into any file read it out. A path outside the project
//! root, or matching `llm.exclude`, is refused by name; a search skips such
//! files silently, as it skips ignored ones.
//!
//! Every output is capped, with a note saying how to read on, so one call
//! cannot flood the context window.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use lattice_agent::EditorAccess;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BoxFuture, SourceTool, ToolError, ToolOutput, ToolSource};
use crate::conversation::ToolKind;
use crate::llm::ToolDescriptor;

/// Lines `read_file` returns at most per call.
pub const MAX_LINES: usize = 2000;
/// Bytes `read_file` returns at most per call.
pub const MAX_BYTES: usize = 200_000;
/// Entries `list_directory` returns at most.
pub const MAX_ENTRIES: usize = 1000;
/// Matches `search` returns at most.
pub const MAX_MATCHES: usize = 200;
/// A matching line is cut to this many characters.
const MAX_MATCH_CHARS: usize = 240;
/// Files larger than this are not searched.
const MAX_SEARCH_FILE: u64 = 2_000_000;

/// The project root a read is confined to, asked per call so a changed root
/// applies at once.
pub type RootFn = Arc<dyn Fn() -> PathBuf + Send + Sync>;
/// The `llm.exclude` glob that matches a path, if any.
pub type ExcludeFn = Arc<dyn Fn(&Path) -> Option<String> + Send + Sync>;

/// Lattice's read tools over an [`EditorAccess`].
pub struct LatticeTools {
    access: EditorAccess,
    root: RootFn,
    exclude: ExcludeFn,
}

impl LatticeTools {
    pub fn new(access: EditorAccess, root: RootFn, exclude: ExcludeFn) -> Self {
        Self {
            access,
            root,
            exclude,
        }
    }

    fn catalogue() -> Vec<SourceTool> {
        let tool = |name: &str, description: &str, kind, input_schema| SourceTool {
            descriptor: ToolDescriptor {
                name: name.into(),
                description: description.into(),
                input_schema,
            },
            kind,
        };
        vec![
            tool(
                "read_file",
                "Read a text file in the project, as the user's editor has it: an open \
                 file's unsaved edits are included. Paths are relative to the project \
                 root. Returns up to 2000 lines; pass start_line to read further.",
                ToolKind::Read,
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path, relative to the project root"},
                        "start_line": {"type": "integer", "minimum": 1, "description": "First line to return, 1-based"},
                        "end_line": {"type": "integer", "minimum": 1, "description": "Last line to return, inclusive"}
                    },
                    "required": ["path"]
                }),
            ),
            tool(
                "list_directory",
                "List a directory in the project. Directories end in `/`. Paths are \
                 relative to the project root; omit path for the root itself.",
                ToolKind::Read,
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "Directory path, relative to the project root"}
                    }
                }),
            ),
            tool(
                "search",
                "Search the project's files for a regular expression, honouring \
                 .gitignore. Open files are searched as the editor has them. Returns \
                 `path:line: text` for up to 200 matching lines.",
                ToolKind::Search,
                json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "A regular expression (Rust syntax)"},
                        "path": {"type": "string", "description": "Directory or file to search under, relative to the project root"},
                        "ignore_case": {"type": "boolean", "description": "Match case-insensitively"}
                    },
                    "required": ["pattern"]
                }),
            ),
        ]
    }

    /// `input` resolved inside the project, or why not.
    fn resolve(&self, input: &str) -> Result<Resolved, ToolError> {
        let root = (self.root)();
        if root.as_os_str().is_empty() {
            return Err(ToolError::Refused(
                "there is no project root to read from".into(),
            ));
        }
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let joined = if Path::new(input).is_absolute() {
            PathBuf::from(input)
        } else {
            root.join(input)
        };
        // Lexically first, so `..` cannot climb out through a path that does
        // not exist; then through symlinks, so a link cannot either.
        let lexical = normalize(&joined);
        let path = std::fs::canonicalize(&lexical).unwrap_or(lexical);
        if !path.starts_with(&root) {
            return Err(ToolError::Refused(format!(
                "`{input}` is outside the project ({})",
                root.display()
            )));
        }
        if let Some(glob) = (self.exclude)(&path) {
            return Err(ToolError::Refused(format!(
                "`{input}` matches llm.exclude ({glob})"
            )));
        }
        Ok(Resolved { path, root })
    }

    async fn read_file(&self, input: Value) -> Result<ToolOutput, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
            start_line: Option<usize>,
            end_line: Option<usize>,
        }
        let args: Args = parse(input)?;
        let at = self.resolve(&args.path)?;
        if at.path.is_dir() {
            return Err(ToolError::Failed(format!(
                "`{}` is a directory; use list_directory",
                args.path
            )));
        }
        let text = self
            .access
            .read_text_file(&at.path)
            .await
            .map_err(|e| ToolError::Failed(read_error(&args.path, &e.to_string())))?;
        Ok(ToolOutput {
            text: slice_lines(&text, args.start_line, args.end_line)?,
        })
    }

    async fn list_directory(&self, input: Value) -> Result<ToolOutput, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            path: Option<String>,
        }
        let args: Args = parse(input)?;
        let shown = args.path.clone().unwrap_or_else(|| ".".into());
        let at = self.resolve(args.path.as_deref().unwrap_or(""))?;
        let exclude = self.exclude.clone();
        let dir = at.path.clone();
        let listed = tokio::task::spawn_blocking(move || list(&dir, &exclude))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?
            .map_err(|e| ToolError::Failed(format!("`{shown}`: {e}")))?;
        Ok(ToolOutput { text: listed })
    }

    async fn search(&self, input: Value) -> Result<ToolOutput, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            pattern: String,
            path: Option<String>,
            #[serde(default)]
            ignore_case: bool,
        }
        let args: Args = parse(input)?;
        let re = regex::RegexBuilder::new(&args.pattern)
            .case_insensitive(args.ignore_case)
            .build()
            .map_err(|e| ToolError::InvalidArguments(format!("pattern: {e}")))?;
        let at = self.resolve(args.path.as_deref().unwrap_or(""))?;
        let open = self.access.open_texts();
        let exclude = self.exclude.clone();
        // Dropping the call (a cancel) stops the walk at its next file.
        let stop = StopOnDrop::default();
        let flag = stop.0.clone();
        let found = tokio::task::spawn_blocking(move || {
            search_files(&at.path, &at.root, &re, &open, &exclude, &flag)
        })
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?;
        drop(stop);
        Ok(ToolOutput { text: found })
    }
}

impl ToolSource for LatticeTools {
    fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>> {
        Box::pin(async { Ok(Self::catalogue()) })
    }

    fn call(&self, name: &str, input: Value) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        let name = name.to_string();
        Box::pin(async move {
            match name.as_str() {
                "read_file" => self.read_file(input).await,
                "list_directory" => self.list_directory(input).await,
                "search" => self.search(input).await,
                other => Err(ToolError::UnknownTool(other.to_string())),
            }
        })
    }
}

struct Resolved {
    path: PathBuf,
    root: PathBuf,
}

/// Sets its flag when dropped, so blocking work can see its caller is gone.
#[derive(Default)]
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// The arguments as `T`, or an error the model can correct from.
fn parse<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T, ToolError> {
    let input = if input.is_null() { json!({}) } else { input };
    if !input.is_object() {
        return Err(ToolError::InvalidArguments(
            "arguments must be a JSON object".into(),
        ));
    }
    serde_json::from_value(input).map_err(|e| ToolError::InvalidArguments(e.to_string()))
}

fn read_error(shown: &str, e: &str) -> String {
    // `EditorAccess` prefixes its own errors; the model needs only the cause.
    let e = e.strip_prefix("editor io error: ").unwrap_or(e);
    if e.contains("No such file") || e.contains("not found") {
        format!("`{shown}` does not exist")
    } else {
        e.to_string()
    }
}

/// `..` and `.` removed without touching the disk.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Lines `start..=end` (1-based) of `text`, capped, with a note when more
/// remain.
fn slice_lines(text: &str, start: Option<usize>, end: Option<usize>) -> Result<String, ToolError> {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let first = start.unwrap_or(1).max(1);
    if total == 0 {
        return Ok("(empty file)".into());
    }
    if first > total {
        return Err(ToolError::InvalidArguments(format!(
            "start_line {first} is past the end ({total} lines)"
        )));
    }
    let wanted_last = end.unwrap_or(total).min(total);
    if wanted_last < first {
        return Err(ToolError::InvalidArguments(format!(
            "end_line {wanted_last} is before start_line {first}"
        )));
    }
    let mut out = String::new();
    let mut last = first - 1;
    for (i, line) in lines[first - 1..wanted_last].iter().enumerate() {
        if i >= MAX_LINES || out.len() + line.len() + 1 > MAX_BYTES {
            break;
        }
        out.push_str(line);
        out.push('\n');
        last = first + i;
    }
    if last < wanted_last {
        out.push_str(&format!(
            "[showing lines {first}-{last} of {total}; pass start_line={} to read on]\n",
            last + 1
        ));
    } else if first > 1 || wanted_last < total {
        out.push_str(&format!("[lines {first}-{last} of {total}]\n"));
    }
    Ok(out)
}

fn list(dir: &Path, exclude: &ExcludeFn) -> std::io::Result<String> {
    let mut entries: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(Result::ok)
        .filter(|e| e.file_name() != ".git" && exclude(&e.path()).is_none())
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                format!("{name}/")
            } else {
                name
            }
        })
        .collect();
    entries.sort();
    let total = entries.len();
    let mut out: String = entries
        .iter()
        .take(MAX_ENTRIES)
        .map(|e| format!("{e}\n"))
        .collect();
    if total == 0 {
        out.push_str("(empty directory)\n");
    } else if total > MAX_ENTRIES {
        out.push_str(&format!("[{MAX_ENTRIES} of {total} entries shown]\n"));
    }
    Ok(out)
}

fn search_files(
    under: &Path,
    root: &Path,
    re: &regex::Regex,
    open: &[(PathBuf, String)],
    exclude: &ExcludeFn,
    stop: &AtomicBool,
) -> String {
    let mut out = String::new();
    let mut matches = 0usize;
    let mut capped = false;
    let walk = ignore::WalkBuilder::new(under)
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();
    'files: for entry in walk {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if exclude(path).is_some() {
            continue;
        }
        let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let text = match open.iter().find(|(p, _)| *p == resolved) {
            Some((_, text)) => text.clone(),
            None => {
                if entry.metadata().map(|m| m.len()).unwrap_or(0) > MAX_SEARCH_FILE {
                    continue;
                }
                let Ok(bytes) = std::fs::read(path) else {
                    continue;
                };
                // Binary: a NUL in the first 8 KiB, as git decides.
                if bytes.iter().take(8192).any(|b| *b == 0) {
                    continue;
                }
                match String::from_utf8(bytes) {
                    Ok(t) => t,
                    Err(_) => continue,
                }
            }
        };
        let shown = resolved.strip_prefix(root).unwrap_or(&resolved).display();
        for (n, line) in text.lines().enumerate() {
            if !re.is_match(line) {
                continue;
            }
            if matches == MAX_MATCHES {
                capped = true;
                break 'files;
            }
            matches += 1;
            let cut: String = line.chars().take(MAX_MATCH_CHARS).collect();
            out.push_str(&format!("{shown}:{}: {cut}\n", n + 1));
        }
    }
    if matches == 0 {
        out.push_str("(no matches)\n");
    } else if capped {
        out.push_str(&format!(
            "[stopped at {MAX_MATCHES} matches; narrow the pattern or the path]\n"
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn lines_are_sliced_and_a_cap_says_how_to_read_on() {
        let text: String = (1..=5).map(|n| format!("l{n}\n")).collect();
        assert_eq!(slice_lines(&text, None, None).unwrap(), text);
        assert_eq!(
            slice_lines(&text, Some(2), Some(3)).unwrap(),
            "l2\nl3\n[lines 2-3 of 5]\n"
        );
        assert!(slice_lines(&text, Some(9), None).is_err());

        let long: String = (1..=MAX_LINES + 5).map(|n| format!("{n}\n")).collect();
        let shown = slice_lines(&long, None, None).unwrap();
        assert!(
            shown.ends_with(&format!(
                "[showing lines 1-{MAX_LINES} of {}; pass start_line={} to read on]\n",
                MAX_LINES + 5,
                MAX_LINES + 1
            )),
            "{}",
            &shown[shown.len() - 120..]
        );
    }

    #[test]
    fn dot_dot_cannot_climb_lexically() {
        assert_eq!(
            normalize(Path::new("/p/src/../../etc/passwd")),
            PathBuf::from("/etc/passwd")
        );
        assert_eq!(normalize(Path::new("/p/./a/b/..")), PathBuf::from("/p/a"));
    }
}
