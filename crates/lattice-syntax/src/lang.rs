//! Languages this crate knows how to parse.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Plain,
    Rust,
    Python,
    JavaScript,
}

impl Lang {
    /// Detect language from a file path's extension.
    pub fn detect_from_path(path: Option<&Path>) -> Self {
        match path
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref()
        {
            Some("rs") => Lang::Rust,
            Some("py") | Some("pyw") => Lang::Python,
            Some("js") | Some("mjs") | Some("cjs") => Lang::JavaScript,
            _ => Lang::Plain,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Lang::Plain => "plain",
            Lang::Rust => "rust",
            Lang::Python => "python",
            Lang::JavaScript => "javascript",
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn detects_rust() {
        assert_eq!(Lang::detect_from_path(Some(&PathBuf::from("foo.rs"))), Lang::Rust);
    }

    #[test]
    fn detects_python() {
        assert_eq!(Lang::detect_from_path(Some(&PathBuf::from("foo.py"))), Lang::Python);
        assert_eq!(Lang::detect_from_path(Some(&PathBuf::from("foo.pyw"))), Lang::Python);
    }

    #[test]
    fn detects_javascript() {
        assert_eq!(Lang::detect_from_path(Some(&PathBuf::from("foo.js"))), Lang::JavaScript);
        assert_eq!(Lang::detect_from_path(Some(&PathBuf::from("foo.mjs"))), Lang::JavaScript);
        assert_eq!(Lang::detect_from_path(Some(&PathBuf::from("foo.cjs"))), Lang::JavaScript);
    }

    #[test]
    fn falls_back_to_plain() {
        assert_eq!(Lang::detect_from_path(None), Lang::Plain);
        assert_eq!(Lang::detect_from_path(Some(&PathBuf::from("README"))), Lang::Plain);
        assert_eq!(Lang::detect_from_path(Some(&PathBuf::from("foo.unknown"))), Lang::Plain);
    }

    #[test]
    fn detection_is_case_insensitive() {
        assert_eq!(Lang::detect_from_path(Some(&PathBuf::from("FOO.RS"))), Lang::Rust);
        assert_eq!(Lang::detect_from_path(Some(&PathBuf::from("Foo.PY"))), Lang::Python);
    }

    #[test]
    fn label_is_distinct_per_lang() {
        assert_eq!(Lang::Plain.label(), "plain");
        assert_eq!(Lang::Rust.label(), "rust");
        assert_eq!(Lang::Python.label(), "python");
        assert_eq!(Lang::JavaScript.label(), "javascript");
    }
}
