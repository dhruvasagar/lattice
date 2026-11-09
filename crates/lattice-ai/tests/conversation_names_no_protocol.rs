//! LL.1 boundary: the neutral `conversation/` module must not name the ACP
//! crate. The ACP adapter (`acp/conversation.rs`) is the only place that does.

#![allow(clippy::unwrap_used)]

use std::path::Path;

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn conversation_module_imports_nothing_from_agent_client_protocol() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/conversation");
    let mut files = Vec::new();
    rust_files(&root, &mut files);
    assert!(!files.is_empty(), "found no sources under {root:?}");
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        assert!(
            !text.contains("agent_client_protocol"),
            "{f:?} names agent_client_protocol; fold it in acp/conversation.rs instead"
        );
    }
}
