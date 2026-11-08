//! LL.20: `:llm-export`, a chat as a file for keeping.
//!
//! Markdown is the transcript itself: the projection the chat buffer shows,
//! with every block open, so what you read is what you keep. Org is built from
//! the model rather than by re-reading that markdown, so a line of prose that
//! happens to read `you:` can never be taken for a turn: turns become
//! headings, fences become `#+begin_src` blocks, and lines org would read as
//! structure inside a block are comma-escaped, as org itself does.
//!
//! Neither is a session: nothing here is read back. Resuming a chat is
//! [`session_file`](super::session_file)'s job.

use crate::conversation::projection::{
    edit_tag, one_line, output_language, permission_tag, status_tag,
};
use crate::conversation::{
    Block, Conversation, FoldDisplay, ProjectionOptions, Role, render_conversation_with,
};

/// The transcript as markdown, every block shown open. Prompts still queued
/// and the last error are not part of the conversation and are left out.
pub fn markdown(conv: &Conversation) -> String {
    let kept = Conversation {
        error: None,
        queued: Vec::new(),
        ..conv.clone()
    };
    render_conversation_with(
        &kept,
        &ProjectionOptions {
            markdown: true,
            reasoning: FoldDisplay::Open,
            tools: FoldDisplay::Open,
        },
    )
}

/// The transcript as org, titled `title`. Each turn is a top-level heading;
/// a markdown heading inside a reply sits one level below its turn.
pub fn org(conv: &Conversation, title: &str) -> String {
    let mut out = format!("#+title: {}\n", one_line(title));
    for turn in &conv.turns {
        let who = match turn.role {
            Role::User => "you",
            Role::Assistant => conv.assistant_label(),
        };
        out.push_str(&format!("\n* {}\n", one_line(who)));
        for block in &turn.blocks {
            match block {
                Block::Text(text) => text_to_org(&mut out, text, 1),
                Block::Reasoning(text) => push_block(&mut out, "example", "", text),
                Block::ToolCall {
                    origin,
                    title,
                    status,
                    kind,
                    input,
                    output,
                    ..
                } => {
                    out.push_str("- \u{25b8} ");
                    if let Some(origin) = origin {
                        out.push_str(&one_line(origin));
                        out.push_str(" \u{b7} ");
                    }
                    out.push_str(&format!("{} [{}]\n", one_line(title), status_tag(status)));
                    if let Some(body) = input {
                        push_block(&mut out, "src", "json", body);
                    }
                    if let Some(body) = output {
                        push_block(&mut out, "src", output_language(*kind, body), body);
                    }
                }
                Block::Source {
                    label, lang, text, ..
                } => {
                    out.push_str(&format!("- \u{25b8} {}\n", one_line(label)));
                    push_block(&mut out, "src", lang, text);
                }
                Block::Edit { path, status } => {
                    out.push_str(&format!(
                        "- \u{270e} {} [{}]\n",
                        one_line(path),
                        edit_tag(status)
                    ));
                }
                Block::Permission {
                    title,
                    description,
                    status,
                    ..
                } => {
                    out.push_str(&format!(
                        "- {} [{}]\n",
                        one_line(title),
                        permission_tag(status)
                    ));
                    if let Some(desc) = description {
                        out.push_str(&format!("  {}\n", one_line(desc)));
                    }
                }
            }
        }
    }
    out
}

/// A `#+begin_<kind>` block holding `body` verbatim but for org's escapes.
/// `src` without a language is an `example` block: org gives a src block's
/// first word the meaning of a language.
fn push_block(out: &mut String, kind: &str, lang: &str, body: &str) {
    let (kind, lang) = match (kind, lang.trim()) {
        ("src", "") | ("src", "text") => ("example", ""),
        (k, l) => (k, l),
    };
    if lang.is_empty() {
        out.push_str(&format!("#+begin_{kind}\n"));
    } else {
        out.push_str(&format!("#+begin_{kind} {lang}\n"));
    }
    for line in body.strip_suffix('\n').unwrap_or(body).split('\n') {
        out.push_str(&escape_in_block(line));
        out.push('\n');
    }
    out.push_str(&format!("#+end_{kind}\n"));
}

/// Org reads a line starting `*` or `#+` as structure even inside a block, and
/// escapes it with a leading comma, which its own export removes. A line that
/// already starts with such a comma gets another, so the first is kept.
fn escape_in_block(line: &str) -> String {
    let body = line.trim_start();
    let structural = ["*", "#+", ",*", ",#+"].iter().any(|p| body.starts_with(p));
    if structural {
        let indent = &line[..line.len() - body.len()];
        format!("{indent},{body}")
    } else {
        line.to_string()
    }
}

/// An open markdown fence: its character, its length, and the block it became.
struct Fence {
    ch: char,
    len: usize,
    kind: &'static str,
}

/// Markdown text as org, under a heading at `level`. Fences become blocks
/// (a fence closes only on a run of its character at least as long, as
/// CommonMark has it, so a tool's long fence survives the code it holds);
/// headings move below the turn; `*` and `+` bullets become `-`, since `* ` at
/// the start of a line is an org heading. Inline code, bold and links are
/// converted; other markup is left as it is.
fn text_to_org(out: &mut String, text: &str, level: usize) {
    let mut fence: Option<Fence> = None;
    for line in text.split('\n') {
        if let Some(open) = &fence {
            if closes(line, open) {
                out.push_str(&format!("#+end_{}\n", open.kind));
                fence = None;
            } else {
                out.push_str(&escape_in_block(line));
                out.push('\n');
            }
            continue;
        }
        if let Some((ch, len, lang)) = opening(line) {
            let kind = if lang.is_empty() || lang == "text" {
                out.push_str("#+begin_example\n");
                "example"
            } else {
                out.push_str(&format!("#+begin_src {lang}\n"));
                "src"
            };
            fence = Some(Fence { ch, len, kind });
            continue;
        }
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
            out.push_str(&"*".repeat(level + hashes));
            out.push(' ');
            out.push_str(&inline(line[hashes..].trim_start()));
            out.push('\n');
            continue;
        }
        let body = line.trim_start();
        let indent = &line[..line.len() - body.len()];
        if let Some(rest) = body.strip_prefix("* ").or_else(|| body.strip_prefix("+ ")) {
            out.push_str(&format!("{indent}- {}\n", inline(rest)));
            continue;
        }
        out.push_str(&inline(line));
        out.push('\n');
    }
    // A reply that stopped inside a fence still closes its block.
    if let Some(open) = fence {
        out.push_str(&format!("#+end_{}\n", open.kind));
    }
}

/// A fence opening: up to three spaces, then three or more backticks or
/// tildes, then the info string, whose first word is the language.
fn opening(line: &str) -> Option<(char, usize, &str)> {
    let body = line.trim_start_matches(' ');
    if line.len() - body.len() > 3 {
        return None;
    }
    let ch = body.chars().next().filter(|c| *c == '`' || *c == '~')?;
    let len = body.chars().take_while(|c| *c == ch).count();
    if len < 3 {
        return None;
    }
    let info = body[len..].trim();
    if ch == '`' && info.contains('`') {
        return None;
    }
    Some((ch, len, info.split_whitespace().next().unwrap_or("")))
}

fn closes(line: &str, open: &Fence) -> bool {
    let body = line.trim();
    body.chars().count() >= open.len && body.chars().all(|c| c == open.ch)
}

/// Inline markdown as org: `` `code` `` to `~code~`, `**bold**` to `*bold*`,
/// `[text](url)` to `[[url][text]]`. Anything unmatched is left as written.
fn inline(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(ch) = rest.chars().next() {
        if ch == '`'
            && let Some(end) = rest[1..].find('`')
            && end > 0
        {
            out.push('~');
            out.push_str(&rest[1..1 + end]);
            out.push('~');
            rest = &rest[end + 2..];
            continue;
        }
        if let Some(body) = rest.strip_prefix("**")
            && let Some(end) = body.find("**")
            && end > 0
        {
            out.push('*');
            out.push_str(&inline(&body[..end]));
            out.push('*');
            rest = &body[end + 2..];
            continue;
        }
        if ch == '['
            && let Some(mid) = rest.find("](")
            && let Some(end) = rest[mid + 2..].find(')')
            && !rest[1..mid].contains(['[', ']'])
        {
            let (text, url) = (&rest[1..mid], &rest[mid + 2..mid + 2 + end]);
            out.push_str(&format!("[[{url}][{text}]]"));
            rest = &rest[mid + 3 + end..];
            continue;
        }
        out.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn org_of(text: &str) -> String {
        let mut out = String::new();
        text_to_org(&mut out, text, 1);
        out
    }

    #[test]
    fn fences_become_blocks_and_a_long_fence_keeps_the_short_one_inside() {
        let md = "before\n````rust\n```\n* not a heading\n#+not a keyword\n````\nafter";
        assert_eq!(
            org_of(md),
            "before\n#+begin_src rust\n```\n,* not a heading\n,#+not a keyword\n#+end_src\nafter\n"
        );
        assert_eq!(
            org_of("~~~\nplain\n~~~"),
            "#+begin_example\nplain\n#+end_example\n"
        );
        assert_eq!(
            org_of("```py\nunclosed"),
            "#+begin_src py\nunclosed\n#+end_src\n",
            "a reply cut off inside a fence still closes"
        );
    }

    #[test]
    fn headings_sit_below_the_turn_and_bullets_are_dashes() {
        assert_eq!(org_of("# Plan\n## Step"), "** Plan\n*** Step\n");
        assert_eq!(
            org_of("* one\n  + two\n- three"),
            "- one\n  - two\n- three\n"
        );
        assert_eq!(
            org_of("#hashtag"),
            "#hashtag\n",
            "not a heading without the space"
        );
    }

    #[test]
    fn inline_code_bold_and_links_convert_and_the_rest_stays() {
        assert_eq!(inline("use `x` here"), "use ~x~ here");
        assert_eq!(inline("**bold** and *em*"), "*bold* and *em*");
        assert_eq!(inline("[docs](https://x.y/z)"), "[[https://x.y/z][docs]]");
        assert_eq!(
            inline("a ` lone tick, [no link]"),
            "a ` lone tick, [no link]"
        );
        assert_eq!(inline("世界 `ü`"), "世界 ~ü~");
    }
}
