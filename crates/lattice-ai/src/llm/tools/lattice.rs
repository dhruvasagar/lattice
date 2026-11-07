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

use super::{BoxFuture, CallContext, SourceTool, ToolError, ToolOutput, ToolSource};
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

/// Closes every review a session opened: sends `Effect::CloseSessionDiffs`.
pub type CloseFn = Arc<dyn Fn(u64) + Send + Sync>;

/// What the edit tools need to put a change in front of the user (LL.17).
#[derive(Clone)]
pub struct Review {
    pub diffs: lattice_diff::ProgrammaticDiffBus,
    /// One diff at a time across the llm layer: a second edit (or rewrite)
    /// waits here until the first is decided.
    pub gate: Arc<tokio::sync::Mutex<()>>,
    pub close: CloseFn,
}

/// Lattice's tools over an [`EditorAccess`]: reads always, edits when a
/// review is available.
pub struct LatticeTools {
    access: EditorAccess,
    root: RootFn,
    exclude: ExcludeFn,
    review: Option<Review>,
}

impl LatticeTools {
    pub fn new(access: EditorAccess, root: RootFn, exclude: ExcludeFn) -> Self {
        Self {
            access,
            root,
            exclude,
            review: None,
        }
    }

    /// With the edit tools, whose changes go through `review`.
    pub fn with_review(mut self, review: Review) -> Self {
        self.review = Some(review);
        self
    }

    fn catalogue(&self) -> Vec<SourceTool> {
        let mut tools = Self::read_catalogue();
        if self.review.is_some() {
            tools.extend(Self::edit_catalogue());
        }
        tools
    }

    fn edit_catalogue() -> Vec<SourceTool> {
        let tool = |name: &str, description: &str, input_schema| SourceTool {
            descriptor: ToolDescriptor {
                name: name.into(),
                description: description.into(),
                input_schema,
            },
            kind: ToolKind::Edit,
        };
        vec![
            tool(
                "edit_file",
                "Replace text in a file in the project. old_string must match the file as                  the editor has it (unsaved edits included) exactly once, unless                  replace_all is set. The user reviews the change as a diff and may accept,                  change or reject it; the result says which.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path, relative to the project root"},
                        "old_string": {"type": "string", "description": "The exact text to replace"},
                        "new_string": {"type": "string", "description": "The text to put in its place"},
                        "replace_all": {"type": "boolean", "description": "Replace every occurrence"}
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            ),
            tool(
                "write_file",
                "Create a file in the project, or replace all of one. The user reviews the                  change as a diff and may accept, change or reject it; the result says which.",
                json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string", "description": "File path, relative to the project root"},
                        "content": {"type": "string", "description": "The whole new content"}
                    },
                    "required": ["path", "content"]
                }),
            ),
        ]
    }

    fn read_catalogue() -> Vec<SourceTool> {
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
        let tools = self.catalogue();
        Box::pin(async move { Ok(tools) })
    }

    fn call(
        &self,
        name: &str,
        input: Value,
        ctx: CallContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        let name = name.to_string();
        Box::pin(async move {
            match name.as_str() {
                "read_file" => self.read_file(input).await,
                "list_directory" => self.list_directory(input).await,
                "search" => self.search(input).await,
                "edit_file" if self.review.is_some() => self.edit_file(input, ctx).await,
                "write_file" if self.review.is_some() => self.write_file(input, ctx).await,
                other => Err(ToolError::UnknownTool(other.to_string())),
            }
        })
    }
}

struct Resolved {
    path: PathBuf,
    root: PathBuf,
}

/// A file as an edit finds it.
struct Target {
    text: String,
    /// Shown in a buffer: the change is applied there, unsaved.
    open: bool,
    exists: bool,
}

/// Marks a review open: tells the request, and on drop (decided or not)
/// tells it the review closed. Dropped undecided, the request was stopped or
/// the chat closed, so the session's diffs are closed too: a diff whose
/// verdict would reach nobody must not stay on screen.
struct InReview {
    ctx: CallContext,
    close: CloseFn,
    decided: bool,
}

impl InReview {
    fn open(ctx: &CallContext, close: &CloseFn) -> Self {
        if let Some(f) = &ctx.on_review {
            f(true);
        }
        Self {
            ctx: ctx.clone(),
            close: close.clone(),
            decided: false,
        }
    }

    fn decided(mut self) {
        self.decided = true;
    }
}

impl Drop for InReview {
    fn drop(&mut self) {
        if let Some(f) = &self.ctx.on_review {
            f(false);
        }
        if !self.decided && self.ctx.origin != 0 {
            (self.close)(self.ctx.origin);
        }
    }
}

impl LatticeTools {
    async fn edit_file(&self, input: Value, ctx: CallContext) -> Result<ToolOutput, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
            old_string: String,
            new_string: String,
            #[serde(default)]
            replace_all: bool,
        }
        let args: Args = parse(input)?;
        if args.old_string.is_empty() {
            return Err(ToolError::InvalidArguments(
                "old_string is empty; use write_file to create or replace a whole file".into(),
            ));
        }
        let at = self.resolve(&args.path)?;
        let target = self.target(&at.path, &args.path).await?;
        if !target.exists {
            return Err(ToolError::Failed(format!(
                "`{}` does not exist; use write_file to create it",
                args.path
            )));
        }
        let count = target.text.matches(&args.old_string).count();
        if count == 0 {
            return Err(ToolError::Failed(format!(
                "old_string was not found in `{}`; read it again, it may have changed",
                args.path
            )));
        }
        if count > 1 && !args.replace_all {
            return Err(ToolError::Failed(format!(
                "old_string occurs {count} times in `{}`; include more of the surrounding \
                 text to pick one, or set replace_all",
                args.path
            )));
        }
        let proposed = if args.replace_all {
            target.text.replace(&args.old_string, &args.new_string)
        } else {
            target.text.replacen(&args.old_string, &args.new_string, 1)
        };
        self.propose(&at, &args.path, target, proposed, ctx).await
    }

    async fn write_file(&self, input: Value, ctx: CallContext) -> Result<ToolOutput, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            path: String,
            content: String,
        }
        let args: Args = parse(input)?;
        let at = self.resolve(&args.path)?;
        if at.path.is_dir() {
            return Err(ToolError::Failed(format!("`{}` is a directory", args.path)));
        }
        let target = self.target(&at.path, &args.path).await?;
        self.propose(&at, &args.path, target, args.content, ctx)
            .await
    }

    /// The file as the user sees it: the buffer's text when open, else the
    /// disk's, else nothing yet.
    async fn target(&self, path: &Path, shown: &str) -> Result<Target, ToolError> {
        if let Some(text) = self.access.buffer_text(path) {
            return Ok(Target {
                text,
                open: true,
                exists: true,
            });
        }
        if !path.exists() {
            return Ok(Target {
                text: String::new(),
                open: false,
                exists: false,
            });
        }
        let text = self
            .access
            .read_text_file(path)
            .await
            .map_err(|e| ToolError::Failed(read_error(shown, &e.to_string())))?;
        Ok(Target {
            text,
            open: false,
            exists: true,
        })
    }

    /// Put `proposed` in front of the user (unless the session trusts edits),
    /// then apply what they accepted to the text the review was of. The file
    /// changing in between refuses: applying would overwrite what changed.
    async fn propose(
        &self,
        at: &Resolved,
        shown: &str,
        target: Target,
        proposed: String,
        ctx: CallContext,
    ) -> Result<ToolOutput, ToolError> {
        if target.exists && proposed == target.text {
            return Ok(ToolOutput {
                text: format!("no change: `{shown}` already holds that"),
            });
        }
        let stale = || {
            ToolError::Failed(format!(
                "`{shown}` changed while the edit waited for review; nothing was applied. \
                 Read it again before proposing a change"
            ))
        };
        let review = self.review.as_ref().ok_or_else(|| {
            ToolError::Refused("edits need a diff review, which is not available here".into())
        })?;
        let accepted = if ctx.trusted {
            proposed.clone()
        } else {
            // Queued behind another review: what it was proposed against may
            // have changed by the time this one opens.
            let _gate = review.gate.lock().await;
            if self.target(&at.path, shown).await?.text != target.text {
                return Err(stale());
            }
            let in_review = InReview::open(&ctx, &review.close);
            let verdict = lattice_agent::review_text(
                &review.diffs,
                lattice_agent::TextReviewRequest {
                    path: at.path.clone(),
                    baseline: target.text.clone(),
                    proposed: proposed.clone(),
                    tab_name: format!("llm edit {shown}"),
                    origin_session: ctx.origin,
                },
            )
            .await;
            in_review.decided();
            match verdict {
                Ok(Some(text)) => text,
                Ok(None) => {
                    return Err(ToolError::Refused(format!(
                        "the user rejected this edit to `{shown}`; nothing changed"
                    )));
                }
                Err(_) => {
                    return Err(ToolError::Refused(format!(
                        "the review of `{shown}` was closed without a decision; nothing changed"
                    )));
                }
            }
        };
        if target.open {
            match self
                .access
                .apply_buffer_text(&at.path, &target.text, &accepted)
                .await
            {
                Ok(true) => {}
                Ok(false) => return Err(stale()),
                Err(_) => {
                    return Err(ToolError::Failed(format!(
                        "`{shown}` was closed during the review; nothing was applied"
                    )));
                }
            }
        } else {
            // Opened in a buffer since: writing the disk under it would leave
            // the buffer showing text the file no longer holds.
            if self.access.buffer_text(&at.path).is_some() {
                return Err(stale());
            }
            let (path, expected, exists, text) = (
                at.path.clone(),
                target.text.clone(),
                target.exists,
                accepted.clone(),
            );
            let wrote = tokio::task::spawn_blocking(move || {
                write_if_unchanged(&path, exists, &expected, &text)
            })
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?;
            match wrote {
                Ok(true) => {}
                Ok(false) => return Err(stale()),
                Err(e) => return Err(ToolError::Failed(format!("`{shown}`: {e}"))),
            }
        }
        let mut text = if target.open {
            format!("applied to `{shown}` in the editor (not saved; the user saves it)")
        } else if target.exists {
            format!("wrote `{shown}`")
        } else {
            format!("created `{shown}`")
        };
        if accepted != proposed {
            text.push_str(
                "; the user changed your proposal before accepting it, so read the file to \
                 see what it holds now",
            );
        }
        Ok(ToolOutput { text })
    }
}

/// Write `text` to `path` if the file is still as it was read (`expected`,
/// or absent when it did not exist). `Ok(false)` when it changed.
fn write_if_unchanged(
    path: &Path,
    existed: bool,
    expected: &str,
    text: &str,
) -> std::io::Result<bool> {
    let now = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let unchanged = match now {
        Some(bytes) => existed && bytes == expected.as_bytes(),
        None => !existed,
    };
    if !unchanged {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, text)?;
    Ok(true)
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
