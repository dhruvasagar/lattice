//! Projection of a [`Conversation`] to buffer text and fold spans (AU‑2, TCF).
//!
//! Pure and protocol-neutral: the `ai-conversation` mode diffs the text this
//! produces into its buffer, and the fold sources in `tool_fold` read the spans.

use crate::conversation::model::{
    Block, Conversation, EditStatus, PermissionStatus, Role, SourceLink, ToolKind, ToolStatus,
};
use crate::conversation::tool_fold::{
    ConversationFold, ConversationFoldKind, reasoning_fold_identity, source_fold_identity,
    tool_fold_identity,
};

/// How a foldable block (reasoning, a tool call) is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FoldDisplay {
    /// In the projection, folded to its head line until opened. The default.
    #[default]
    Closed,
    /// In the projection and unfolded.
    Open,
    /// Left out of the projection. The store keeps the block, so changing this
    /// back brings it back.
    Hidden,
}

impl FoldDisplay {
    /// Parse a config value (`closed`, `open`, `hidden`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "closed" => Some(Self::Closed),
            "open" => Some(Self::Open),
            "hidden" => Some(Self::Hidden),
            _ => None,
        }
    }
}

/// What differs between the conversations this projection serves. The default
/// is `:opencode-acp`'s display (plain indented blocks, folds closed); the
/// native LLM chat turns the markdown form on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProjectionOptions {
    /// Keep the text valid markdown at every moment: close an unclosed fence,
    /// and render a tool call as a head line over fenced bodies.
    pub markdown: bool,
    pub reasoning: FoldDisplay,
    pub tools: FoldDisplay,
}

/// Where a buffer's [`ProjectionOptions`] come from, read on every projection so
/// a changed setting applies to the next one.
#[derive(Clone)]
pub struct ProjectionSource(std::sync::Arc<dyn Fn() -> ProjectionOptions + Send + Sync>);

impl ProjectionSource {
    pub fn new(f: impl Fn() -> ProjectionOptions + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(f))
    }

    pub fn fixed(options: ProjectionOptions) -> Self {
        Self::new(move || options)
    }

    pub fn get(&self) -> ProjectionOptions {
        (self.0)()
    }
}

impl Default for ProjectionSource {
    fn default() -> Self {
        Self::fixed(ProjectionOptions::default())
    }
}

/// Render the whole conversation to plain text. Pure; the projection diffs this
/// against the buffer to compute a minimal edit. Thin wrapper over
/// [`project_conversation`] — the text half.
pub fn render_conversation(conv: &Conversation) -> String {
    render_conversation_with(conv, &ProjectionOptions::default())
}

/// [`render_conversation`] under explicit options.
pub fn render_conversation_with(conv: &Conversation, opts: &ProjectionOptions) -> String {
    project_conversation_with(conv, opts).0
}

/// Output written so far and the line the next push lands on, kept together so
/// a fold's line range can never drift from the text.
struct Sink {
    out: String,
    line: u32,
}

impl Sink {
    fn push(&mut self, s: &str) {
        self.out.push_str(s);
    }

    /// End the current line.
    fn end_line(&mut self) {
        self.out.push('\n');
        self.line += 1;
    }

    fn line(&mut self, s: &str) {
        self.push(s);
        self.end_line();
    }
}

/// TCF: project the conversation to `(transcript_text, fold_spans)` in a single
/// pass, so a fold's line range can never drift from the rendered layout. The
/// text half is what the drain diffs into the buffer; the span half is what the
/// [`ToolCallFoldSource`](crate::conversation::tool_fold::ToolCallFoldSource) and
/// [`ReasoningFoldSource`](crate::conversation::tool_fold::ReasoningFoldSource) read.
///
/// A tool call with captured `input`/`output` gains indented detail rows under
/// its `▸ summary [status]` head; the fold spans the head through the last
/// detail row (head stays visible when closed, detail hides). Each multi-line
/// reasoning block folds the same way. Detail-less tool calls and single-line
/// reasoning yield no span — a 1-line fold is a `z*` no-op.
///
/// Fold line indices are 0-based on the split-on-`\n` grid and align with the
/// buffer's line numbers: the transcript occupies buffer lines `0..N` and the
/// prompt marker is appended after it, so it never shifts a transcript line.
pub fn project_conversation(conv: &Conversation) -> (String, Vec<ConversationFold>) {
    project_conversation_with(conv, &ProjectionOptions::default())
}

/// [`project_conversation`] under explicit options.
pub fn project_conversation_with(
    conv: &Conversation,
    opts: &ProjectionOptions,
) -> (String, Vec<ConversationFold>) {
    let layout = layout(conv, opts);
    (layout.text, layout.folds)
}

/// LL.10b: the source a [`Block::Source`] spanning `line` links back to, in the
/// projection `opts` give. Re-runs the layout pass, so the answer can never
/// disagree with the text; it is pure and runs only on a `gf`.
pub fn source_at_line(
    conv: &Conversation,
    opts: &ProjectionOptions,
    line: u32,
) -> Option<SourceLink> {
    layout(conv, opts)
        .sources
        .into_iter()
        .find(|(start, end, _)| (*start..=*end).contains(&line))
        .map(|(_, _, link)| link)
}

/// LL.19: the index of the turn whose text `line` falls in, as `opts` lays the
/// transcript out. `None` on the prompt and anything below the transcript.
pub fn turn_at_line(conv: &Conversation, opts: &ProjectionOptions, line: u32) -> Option<usize> {
    let laid = layout(conv, opts);
    if line >= laid.text.matches('\n').count() as u32 {
        return None;
    }
    laid.turn_starts.iter().rposition(|&start| start <= line)
}

/// One layout pass: the text, its folds, and the line span of each linked
/// source block.
struct Layout {
    text: String,
    folds: Vec<ConversationFold>,
    sources: Vec<(u32, u32, SourceLink)>,
    /// The line each turn's label starts on, in turn order.
    turn_starts: Vec<u32>,
}

fn layout(conv: &Conversation, opts: &ProjectionOptions) -> Layout {
    let mut sources: Vec<(u32, u32, SourceLink)> = Vec::new();
    let mut source_ordinal: usize = 0;
    let mut s = Sink {
        out: String::new(),
        line: 0,
    };
    let mut folds: Vec<ConversationFold> = Vec::new();
    // Reasoning blocks carry no wire id; their fold identity is keyed on this
    // document-order ordinal, incremented for every reasoning block regardless
    // of whether it is foldable or shown, so a given block's ordinal stays
    // stable.
    let mut reasoning_ordinal: usize = 0;
    let mut turn_starts: Vec<u32> = Vec::with_capacity(conv.turns.len());
    for turn in &conv.turns {
        turn_starts.push(s.line);
        let who = match turn.role {
            Role::User => "you",
            Role::Assistant => conv.assistant_label(),
        };
        s.push(who);
        s.line(":");
        label_guard(&mut s, opts.markdown);
        for block in &turn.blocks {
            match block {
                Block::Text(text) => push_text(&mut s, text, opts.markdown),
                Block::Reasoning(text) => {
                    let ordinal = reasoning_ordinal;
                    reasoning_ordinal += 1;
                    // An empty block holds only round-trip data (a signature
                    // a provider gave without thought text): nothing to show.
                    if opts.reasoning == FoldDisplay::Hidden || text.is_empty() {
                        continue;
                    }
                    let start = s.line;
                    for l in text.split('\n') {
                        s.push("  \u{2502} ");
                        s.line(l);
                    }
                    push_fold(
                        &mut folds,
                        start,
                        s.line - 1,
                        reasoning_fold_identity(ordinal),
                        ConversationFoldKind::Reasoning,
                    );
                }
                Block::ToolCall {
                    id,
                    origin,
                    title,
                    status,
                    kind,
                    input,
                    output,
                } => {
                    if opts.tools == FoldDisplay::Hidden {
                        continue;
                    }
                    let start = s.line;
                    if opts.markdown {
                        push_markdown_tool(
                            &mut s,
                            origin.as_deref(),
                            title,
                            status,
                            *kind,
                            input.as_deref(),
                            output.as_deref(),
                        );
                    } else {
                        push_indented_tool(&mut s, title, status, input, output);
                    }
                    push_fold(
                        &mut folds,
                        start,
                        s.line - 1,
                        tool_fold_identity(id),
                        ConversationFoldKind::ToolCall,
                    );
                }
                Block::Source {
                    label,
                    lang,
                    text,
                    link,
                } => {
                    let ordinal = source_ordinal;
                    source_ordinal += 1;
                    let start = s.line;
                    s.push("\u{25b8} ");
                    s.line(&one_line(label));
                    if opts.markdown {
                        push_fenced(&mut s, lang, text);
                    } else {
                        for l in text.split('\n') {
                            s.push("    ");
                            s.line(l);
                        }
                    }
                    if let Some(link) = link {
                        sources.push((start, s.line - 1, *link));
                    }
                    push_fold(
                        &mut folds,
                        start,
                        s.line - 1,
                        source_fold_identity(ordinal),
                        ConversationFoldKind::Source,
                    );
                }
                Block::Edit { path, status } => {
                    s.push("  \u{270e} ");
                    s.push(path);
                    s.push(" [");
                    s.push(edit_tag(status));
                    s.line("]");
                }
                Block::Permission {
                    title,
                    description,
                    options,
                    status,
                    ..
                } => {
                    s.push("  ");
                    s.push(permission_prefix(status));
                    s.push(title);
                    s.push(" [");
                    s.push(permission_tag(status));
                    s.line("]");
                    if let Some(desc) = description {
                        s.push("    ");
                        s.line(desc);
                    }
                    if status == &PermissionStatus::Pending {
                        // PU-B.2: the inline block is a record, not the
                        // interaction surface — the popup menu (`ai-permission-mode`)
                        // owns the numbered selectors. List the offered options
                        // without the old `(a)/(A)/(r)/(R)` key hints, which named
                        // chords that were never bound.
                        for (i, opt) in options.iter().enumerate() {
                            s.line(&format!("    {}: {}", i + 1, opt.name));
                        }
                    }
                }
            }
        }
        s.end_line();
    }
    // Prompts waiting behind a streaming reply, then the last failure. Both
    // sit after the turns so the history above them never shifts.
    for text in &conv.queued {
        s.line("you (queued):");
        label_guard(&mut s, opts.markdown);
        push_text(&mut s, text, opts.markdown);
        s.end_line();
    }
    if let Some(err) = &conv.error {
        for (i, l) in err.split('\n').enumerate() {
            s.push(if i == 0 { "! " } else { "  " });
            s.line(l);
        }
        s.end_line();
    }
    Layout {
        text: s.out,
        folds,
        sources,
        turn_starts,
    }
}

/// In markdown form a blank line after a speaker label, so a first line of
/// `---` or `===` cannot turn the label into a setext heading.
fn label_guard(s: &mut Sink, markdown: bool) {
    if markdown {
        s.line("");
    }
}

/// `text` on one line: newlines become spaces, so a title or origin cannot
/// break the head line open.
pub(crate) fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

/// A fold needs an interior: a 1-line region is a `z*` no-op.
fn push_fold(
    folds: &mut Vec<ConversationFold>,
    start: u32,
    end: u32,
    identity: u64,
    kind: ConversationFoldKind,
) {
    if end > start {
        folds.push(ConversationFold {
            start_line: start,
            end_line: end,
            identity,
            kind,
        });
    }
}

/// Text lines, and in markdown form a closing fence when the text leaves one
/// open, so a half-received code block cannot colour what follows it (the next
/// block, and the prompt) as code.
fn push_text(s: &mut Sink, text: &str, markdown: bool) {
    for l in text.split('\n') {
        s.line(l);
    }
    if markdown && let Some(close) = unclosed_fence(text) {
        s.line(&close);
    }
}

/// The ACP form: a `▸` head with indented `input:` / `output:` detail rows.
fn push_indented_tool(
    s: &mut Sink,
    title: &str,
    status: &ToolStatus,
    input: &Option<String>,
    output: &Option<String>,
) {
    s.push("  \u{25b8} ");
    s.push(title);
    s.push(" [");
    s.push(status_tag(status));
    s.line("]");
    for (label, body) in [("input", input), ("output", output)] {
        let Some(body) = body else { continue };
        s.push("    ");
        s.push(label);
        s.line(":");
        for l in body.split('\n') {
            s.push("      ");
            s.line(l);
        }
    }
}

/// The markdown form: a head line outside any fence, so a folded block still
/// reads as a sentence, over fenced bodies that take their highlighting from
/// the grammar's injections.
fn push_markdown_tool(
    s: &mut Sink,
    origin: Option<&str>,
    title: &str,
    status: &ToolStatus,
    kind: ToolKind,
    input: Option<&str>,
    output: Option<&str>,
) {
    s.push("\u{25b8} ");
    if let Some(origin) = origin {
        s.push(&one_line(origin));
        s.push(" \u{b7} ");
    }
    s.push(&one_line(title));
    s.push(" [");
    s.push(status_tag(status));
    s.line("]");
    if let Some(body) = input {
        push_fenced(s, "json", body);
    }
    if let Some(body) = output {
        push_fenced(s, output_language(kind, body), body);
    }
}

/// The language of a tool's output: by the tool's kind where that says
/// something, then `json` if it opens like an object or array, else `text`.
/// No parse: this runs for every tool output on every projection, and a
/// 100 KB body is not worth decoding to pick a highlighting language.
pub(crate) fn output_language(kind: ToolKind, body: &str) -> &'static str {
    match kind {
        ToolKind::Edit => "diff",
        ToolKind::Execute => "bash",
        _ if matches!(
            body.bytes().find(|b| !b.is_ascii_whitespace()),
            Some(b'{' | b'[')
        ) =>
        {
            "json"
        }
        _ => "text",
    }
}

/// `body` in a fence longer than any backtick run it contains, so its content
/// can never close the fence early.
fn push_fenced(s: &mut Sink, lang: &str, body: &str) {
    let fence = "`".repeat(longest_backtick_run(body).max(2) + 1);
    s.push(&fence);
    s.line(lang);
    for l in body.split('\n') {
        s.line(l);
    }
    s.line(&fence);
}

fn longest_backtick_run(s: &str) -> usize {
    let mut best = 0;
    let mut cur = 0;
    for c in s.chars() {
        if c == '`' {
            cur += 1;
            best = best.max(cur);
        } else {
            cur = 0;
        }
    }
    best
}

/// The closing line for a fence `text` leaves open, if any (CommonMark: an
/// opener is 3+ backticks or tildes under at most 3 spaces of indent; a closer
/// is the same character, at least as long, with nothing after it).
fn unclosed_fence(text: &str) -> Option<String> {
    let mut open: Option<(char, usize)> = None;
    for line in text.split('\n') {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if indent > 3 {
            continue;
        }
        let rest = &line[indent..];
        let Some(ch) = rest.chars().next().filter(|c| *c == '`' || *c == '~') else {
            continue;
        };
        let run = rest.chars().take_while(|c| *c == ch).count();
        if run < 3 {
            continue;
        }
        let after = &rest[run..];
        match open {
            Some((och, olen)) if och == ch && run >= olen && after.trim().is_empty() => {
                open = None;
            }
            Some(_) => {}
            // A backtick opener's info string may not contain a backtick.
            None if ch == '`' && after.contains('`') => {}
            None => open = Some((ch, run)),
        }
    }
    open.map(|(ch, len)| ch.to_string().repeat(len))
}

pub(crate) fn status_tag(status: &ToolStatus) -> &'static str {
    use ToolStatus::*;
    match status {
        Pending => "pending",
        Running => "running",
        Ok => "ok",
        Err => "error",
    }
}

pub(crate) fn edit_tag(status: &EditStatus) -> &'static str {
    use EditStatus::*;
    match status {
        Proposed => "proposed",
        Accepted => "accepted",
        Rejected => "rejected",
    }
}

fn permission_prefix(status: &PermissionStatus) -> &'static str {
    use PermissionStatus::*;
    match status {
        Pending => "\u{25cc} ", // ◌
        Allowed => "\u{2713} ", // ✓
        Denied => "\u{2717} ",  // ✗
    }
}

pub(crate) fn permission_tag(status: &PermissionStatus) -> &'static str {
    use PermissionStatus::*;
    match status {
        Pending => "pending",
        Allowed => "allowed",
        Denied => "denied",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use crate::conversation::PermissionStatus as PS;
    use crate::conversation::model::{PermissionKind, PermissionOption, Turn};

    fn text_turn(role: Role, text: &str) -> Turn {
        Turn {
            role,
            blocks: vec![Block::Text(text.to_string())],
        }
    }

    fn test_permission_option(
        id: &'static str,
        name: &'static str,
        kind: PermissionKind,
    ) -> PermissionOption {
        PermissionOption::new(id, name, kind)
    }

    use crate::conversation::model::SourceLink;
    use crate::conversation::tool_fold::ConversationFoldKind;

    /// LL.19: every line of a turn, its label included, names that turn;
    /// past the transcript (the prompt) names none.
    #[test]
    fn turn_at_line_names_the_turn_a_line_belongs_to() {
        let conv = Conversation {
            turns: vec![
                text_turn(Role::User, "one"),
                text_turn(Role::Assistant, "two\nlines"),
                text_turn(Role::User, "three"),
            ],
            ..Default::default()
        };
        let opts = ProjectionOptions::default();
        let text = render_conversation_with(&conv, &opts);
        let lines: Vec<&str> = text.lines().collect();
        let line_of = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap() as u32;
        assert_eq!(turn_at_line(&conv, &opts, 0), Some(0));
        assert_eq!(turn_at_line(&conv, &opts, line_of("one")), Some(0));
        assert_eq!(turn_at_line(&conv, &opts, line_of("two")), Some(1));
        assert_eq!(turn_at_line(&conv, &opts, line_of("lines")), Some(1));
        assert_eq!(turn_at_line(&conv, &opts, line_of("three")), Some(2));
        let past = text.matches('\n').count() as u32;
        assert_eq!(turn_at_line(&conv, &opts, past), None, "the prompt line");
        assert_eq!(turn_at_line(&Conversation::default(), &opts, 0), None);
    }

    #[test]
    fn renders_turn_headers_and_blocks() {
        let conv = Conversation {
            turns: vec![
                text_turn(Role::User, "refactor parse_args"),
                Turn {
                    role: Role::Assistant,
                    blocks: vec![
                        Block::Text("I'll extract a helper.".to_string()),
                        Block::ToolCall {
                            id: "t1".to_string(),
                            origin: None,
                            title: "edit parse.rs".to_string(),
                            status: ToolStatus::Running,
                            kind: Default::default(),
                            input: None,
                            output: None,
                        },
                    ],
                },
            ],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("you:\nrefactor parse_args\n"));
        assert!(text.contains("opencode:\nI'll extract a helper.\n"));
        assert!(text.contains("\u{25b8} edit parse.rs [running]"));
    }

    // ── TCF: tool-call / reasoning fold projection ──

    fn detailed_tool_call() -> Block {
        Block::ToolCall {
            id: "tc-1".to_string(),
            origin: None,
            title: "bash".to_string(),
            status: ToolStatus::Ok,
            kind: Default::default(),
            input: Some("{\n  \"cmd\": \"echo hi\"\n}".to_string()),
            output: Some("\"hi\"".to_string()),
        }
    }

    /// A detailed tool call renders indented `input:` / `output:` rows beneath
    /// its summary, and yields one fold spanning the summary head through the
    /// last detail row — keyed on the tool-call id. The fold's `start_line`
    /// lands on the `▸` summary line in the rendered text (head stays visible
    /// when closed; the detail rows below hide).
    #[test]
    fn tool_call_detail_rows_render_and_fold_from_the_summary_head() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![detailed_tool_call()],
            }],
            ..Default::default()
        };
        let (text, folds) = project_conversation(&conv);
        assert!(text.contains("\u{25b8} bash [ok]"), "summary head: {text}");
        assert!(text.contains("    input:\n"), "input label: {text}");
        assert!(
            text.contains("      \"cmd\": \"echo hi\""),
            "indented input body: {text}"
        );
        assert!(text.contains("    output:\n"), "output label: {text}");

        assert_eq!(folds.len(), 1);
        let f = &folds[0];
        assert_eq!(f.kind, ConversationFoldKind::ToolCall);
        assert_eq!(f.identity, tool_fold_identity("tc-1"));
        // The fold head is the summary line; the detail rows are its interior.
        let lines: Vec<&str> = text.split('\n').collect();
        assert!(
            lines[f.start_line as usize].contains("\u{25b8} bash [ok]"),
            "start_line is the summary head: {:?}",
            lines[f.start_line as usize],
        );
        assert!(f.end_line > f.start_line, "detail rows form the interior");
        // The last folded row is a detail row (the output body), not blank.
        assert!(
            lines[f.end_line as usize].contains("\"hi\""),
            "end is the last detail row"
        );
    }

    /// A tool call with no captured detail renders only its summary and yields
    /// no fold — a 1-line region is a `z*` no-op.
    #[test]
    fn detail_less_tool_call_yields_no_fold() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::ToolCall {
                    id: "t1".to_string(),
                    origin: None,
                    title: "think".to_string(),
                    status: ToolStatus::Running,
                    kind: Default::default(),
                    input: None,
                    output: None,
                }],
            }],
            ..Default::default()
        };
        let (_text, folds) = project_conversation(&conv);
        assert!(folds.is_empty(), "no detail rows → nothing to fold");
    }

    /// A multi-line reasoning block folds (identity keyed on its document-order
    /// ordinal); a single-line one does not.
    #[test]
    fn reasoning_folds_only_when_multiline() {
        let multi = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Reasoning(
                    "first thought\nsecond thought".to_string(),
                )],
            }],
            ..Default::default()
        };
        let (text, folds) = project_conversation(&multi);
        assert!(
            text.contains("  \u{2502} first thought"),
            "reasoning prefix: {text}"
        );
        assert_eq!(folds.len(), 1);
        assert_eq!(folds[0].kind, ConversationFoldKind::Reasoning);
        assert_eq!(folds[0].identity, reasoning_fold_identity(0));

        let single = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Reasoning("just one line".to_string())],
            }],
            ..Default::default()
        };
        assert!(
            project_conversation(&single).1.is_empty(),
            "a single reasoning line is not foldable",
        );
    }

    /// Fold line numbers are on the same split-on-`\n` grid the drain diffs and
    /// the buffer uses: `render_conversation` and `project_conversation` share
    /// one pass, so the text and the spans can never disagree.
    #[test]
    fn render_and_project_share_the_same_text() {
        let conv = Conversation {
            turns: vec![
                text_turn(Role::User, "run it"),
                Turn {
                    role: Role::Assistant,
                    blocks: vec![
                        Block::Reasoning("planning\nthe\napproach".to_string()),
                        detailed_tool_call(),
                    ],
                },
            ],
            ..Default::default()
        };
        assert_eq!(render_conversation(&conv), project_conversation(&conv).0);
    }

    // ── AUX‑1: permission block rendering tests ──

    #[test]
    fn render_permission_block_pending_shows_circle_and_options() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Permission {
                    id: "perm-1".to_string(),
                    title: "Allow cargo test?".to_string(),
                    description: None,
                    options: vec![
                        test_permission_option("a1", "Allow once", PermissionKind::AllowOnce),
                        test_permission_option("r1", "Reject", PermissionKind::RejectOnce),
                    ],
                    status: PS::Pending,
                }],
            }],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("\u{25cc} Allow cargo test? [pending]"));
        // PU-B.2: the inline record lists options without the old key hints —
        // the popup menu owns the numbered selectors.
        assert!(text.contains("1: Allow once"));
        assert!(text.contains("2: Reject"));
        assert!(!text.contains("(a)"), "no more misleading key hints");
    }

    #[test]
    fn render_permission_block_allowed_shows_checkmark() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Permission {
                    id: "perm-1".to_string(),
                    title: "Allow cargo test?".to_string(),
                    description: None,
                    options: vec![],
                    status: PS::Allowed,
                }],
            }],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("\u{2713} Allow cargo test? [allowed]"));
        assert!(!text.contains("1:"), "no options after resolution");
    }

    #[test]
    fn render_permission_block_denied_shows_x() {
        let conv = Conversation {
            turns: vec![Turn {
                role: Role::Assistant,
                blocks: vec![Block::Permission {
                    id: "perm-1".to_string(),
                    title: "Allow cargo test?".to_string(),
                    description: None,
                    options: vec![],
                    status: PS::Denied,
                }],
            }],
            ..Default::default()
        };
        let text = render_conversation(&conv);
        assert!(text.contains("\u{2717} Allow cargo test? [denied]"));
    }

    #[test]
    fn the_assistant_label_defaults_to_opencode_and_can_be_a_model_name() {
        let mut conv = Conversation {
            turns: vec![text_turn(Role::Assistant, "hi")],
            ..Default::default()
        };
        assert!(render_conversation(&conv).starts_with("opencode:\n"));
        conv.assistant_label = Some("claude-x".into());
        assert!(render_conversation(&conv).starts_with("claude-x:\n"));
    }

    #[test]
    fn queued_prompts_and_the_error_follow_the_turns() {
        let conv = Conversation {
            turns: vec![text_turn(Role::User, "q")],
            queued: vec!["next\nline".into()],
            error: Some("boom\nmore".into()),
            ..Default::default()
        };
        assert_eq!(
            render_conversation(&conv),
            "you:\nq\n\nyou (queued):\nnext\nline\n\n! boom\n  more\n\n"
        );
    }

    /// The drain clears the prompt exactly when this rises, so a queued prompt
    /// becoming a turn must leave it unchanged.
    fn asked(lines: usize) -> Conversation {
        let text = (0..lines)
            .map(|n| format!("fn f{n}() {{}}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut c = Conversation::default();
        c.push_user_source(
            Block::Source {
                label: "foo.rs:3-4".into(),
                lang: "rust".into(),
                text,
                link: Some(SourceLink {
                    buffer: lattice_core::BufferId(7),
                    line: 2,
                }),
            },
            "explain",
        );
        c
    }

    const MD: ProjectionOptions = ProjectionOptions {
        markdown: true,
        reasoning: FoldDisplay::Closed,
        tools: FoldDisplay::Closed,
    };

    /// LL.10b: what an ask sent is a head line naming its place over the text
    /// in a fence of its language, then the typed instruction.
    #[test]
    fn a_source_block_is_a_head_line_over_a_fence_then_the_instruction() {
        let (text, folds) = project_conversation_with(&asked(2), &MD);
        assert!(
            text.starts_with(
                "you:\n\n\u{25b8} foo.rs:3-4\n```rust\nfn f0() {}\nfn f1() {}\n```\nexplain\n"
            ),
            "{text:?}"
        );
        assert_eq!(folds.len(), 1);
        assert_eq!(folds[0].kind, ConversationFoldKind::Source);
        assert_eq!((folds[0].start_line, folds[0].end_line), (2, 6));
    }

    /// `gf` finds the block from any of its lines, and nothing outside it.
    #[test]
    fn a_line_of_the_source_block_links_back_and_others_do_not() {
        let conv = asked(2);
        let link = SourceLink {
            buffer: lattice_core::BufferId(7),
            line: 2,
        };
        for line in 2..=6 {
            assert_eq!(source_at_line(&conv, &MD, line), Some(link), "line {line}");
        }
        assert_eq!(source_at_line(&conv, &MD, 0), None);
        assert_eq!(source_at_line(&conv, &MD, 7), None, "the instruction");
    }

    #[test]
    fn submission_count_rises_on_a_submit_and_not_on_a_dequeue() {
        let mut conv = Conversation::default();
        assert_eq!(conv.submission_count(), 0);
        conv.push_user_text("a");
        conv.queued.push("b".into());
        assert_eq!(conv.submission_count(), 2);
        let next = conv.queued.remove(0);
        conv.push_user_text(&next);
        assert_eq!(conv.submission_count(), 2);
        conv.extend_text(Role::Assistant, "agent text");
        assert_eq!(conv.submission_count(), 2, "agent turns do not count");
    }
}
