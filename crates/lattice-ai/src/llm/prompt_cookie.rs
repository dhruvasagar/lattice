//! `@profile` cookies in a typed prompt (design §4, "Overriding on the go").
//!
//! `@writer` in a prompt names a profile for **that one request**. Three rules
//! keep it from becoming a way to change settings by accident or by paste:
//!
//! - **Only known names match.** `@unknown`, `@mention` and an email address
//!   are ordinary text and are sent as typed.
//! - **Only the text the user typed is scanned.** The caller passes the typed
//!   instruction and never the selected or pasted source, so a file that
//!   happens to contain `@coding` cannot select a profile.
//! - **The first match wins**, and a code fence is skipped: a `@decorator` in a
//!   pasted snippet is code, not a cookie.
//!
//! The cookie stays in the text. The transcript shows what was typed, and the
//! model sees the same words the user did.

/// The first known profile named by an `@name` token in `typed`, if any.
pub fn find(typed: &str, known: &[String]) -> Option<String> {
    if known.is_empty() {
        return None;
    }
    let mut in_fence = false;
    for line in typed.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if let Some(name) = find_in_line(line, known) {
            return Some(name);
        }
    }
    None
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '-' | '_')
}

fn find_in_line(line: &str, known: &[String]) -> Option<String> {
    let mut prev: Option<char> = None;
    for (i, c) in line.char_indices() {
        // `@` opens a cookie only at the start of a word, so `me@writer.com`
        // is an address and not a request for the `writer` profile.
        let at_word_start = prev.is_none_or(|p| p.is_whitespace() || "([{\"'".contains(p));
        if c == '@' && at_word_start {
            let name: String = line[i + 1..]
                .chars()
                .take_while(|c| is_name_char(*c))
                .collect();
            if known.contains(&name) {
                return Some(name);
            }
        }
        prev = Some(c);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known() -> Vec<String> {
        vec!["writer".into(), "coding".into()]
    }

    #[test]
    fn a_known_name_matches_anywhere_in_a_line() {
        assert_eq!(
            find("@writer tighten this", &known()).as_deref(),
            Some("writer")
        );
        assert_eq!(
            find("please, @coding fix it.", &known()).as_deref(),
            Some("coding")
        );
        assert_eq!(find("(@writer)", &known()).as_deref(), Some("writer"));
    }

    #[test]
    fn an_unknown_name_is_not_a_cookie() {
        assert_eq!(find("@unknown do it", &known()), None);
        assert_eq!(
            find("@writers do it", &known()),
            None,
            "a prefix is not a match"
        );
    }

    #[test]
    fn an_address_or_a_mid_word_at_is_not_a_cookie() {
        assert_eq!(find("mail me@writer.com", &known()), None);
        assert_eq!(find("a@coding", &known()), None);
    }

    #[test]
    fn a_fenced_block_is_skipped() {
        assert_eq!(find("look:\n```\n@coding\n```\nthanks", &known()), None);
        assert_eq!(
            find("```\n@coding\n```\n@writer", &known()).as_deref(),
            Some("writer")
        );
    }

    #[test]
    fn the_first_match_wins_and_no_profiles_means_no_match() {
        assert_eq!(
            find("@coding then @writer", &known()).as_deref(),
            Some("coding")
        );
        assert_eq!(find("@writer", &[]), None);
    }
}
