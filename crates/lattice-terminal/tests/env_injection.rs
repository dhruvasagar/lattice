//! I5.0: `SpawnConfig.env` reaches the spawned child.
//!
//! Spawns `sh -c 'printf MARKER=$LATTICE_I5_ENV'` with the variable injected via
//! `SpawnConfig.env` and reads it back out of the published terminal snapshot —
//! proving the env pair is threaded through `CommandBuilder::env` into the
//! child's environment. The reader runs on a detached OS thread (no tokio
//! runtime needed), so this is a plain `#[test]` with a bounded poll (no fixed
//! sleep) to stay robust under load.

use std::time::{Duration, Instant};

use lattice_terminal::SpawnConfig;

/// Assemble the full terminal grid into a single string.
fn grid_text(snap: &lattice_terminal::TerminalSnapshot) -> String {
    let mut out = String::with_capacity(snap.rows as usize * snap.cols as usize);
    for row in 0..snap.rows {
        for col in 0..snap.cols {
            out.push(snap.cell_at(row, col).ch);
        }
    }
    out
}

#[test]
fn injected_env_reaches_the_child() {
    let cfg = SpawnConfig {
        program: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            "printf 'MARKER=%s' \"$LATTICE_I5_ENV\"".to_string(),
        ],
        cwd: None,
        env: vec![("LATTICE_I5_ENV".to_string(), "ok".to_string())],
        rows: 24,
        cols: 80,
        scrollback_lines: 1000,
        paint_request: None,
    };

    let handles = lattice_terminal::spawn(cfg).expect("spawn sh");

    // Poll the snapshot until the child's output appears (bounded ~3s).
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let text = grid_text(&handles.snapshot.load());
        if text.contains("MARKER=ok") {
            return; // env pair reached the child
        }
        assert!(
            Instant::now() < deadline,
            "injected env var never surfaced in the child output; grid was: {text:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}
