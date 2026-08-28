//! OM.A1 agenda fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the
//! `agenda-source-plugin` world, driving the agenda producer actor
//! (`agenda_task.rs`) through real host→guest calls:
//!
//!   - `extensions` declares `["ORG"]` — deliberately upper-cased and
//!     dot-prefixed in one entry, so the host's normalisation is proven
//!     against a guest rather than only against a unit test;
//!   - `scan` returns one row per line beginning `* TODO`, with a `sort_key`
//!     read out of the line so the host test can assert the cross-file sort
//!     using data only the guest produced;
//!   - it keeps a **counter across `scan` calls** and reports it in the row's
//!     label, so a test can prove `begin` resets per-scan state;
//!   - a file containing `BROKEN` returns the WIT typed `err`, exercising the
//!     skip-this-file-and-continue path (§8) with a real guest error.

wit_bindgen::generate!({
    world: "agenda-source-plugin",
    path: "../../../../../wit",
});

// Per-scan state. Single-threaded guest with its calls serialised by the
// host's per-plugin actor, so a `thread_local` is the whole story — and this
// is exactly the state `begin` exists to clear.
thread_local! {
    static FILES_SEEN: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct Component;

impl Guest for Component {
    fn extensions() -> Vec<String> {
        // `.ORG` rather than `org`: the host lowercases and strips the dot, and
        // a fixture that only ever sends the already-clean spelling would let
        // that normalisation rot unnoticed.
        vec![".ORG".to_string()]
    }

    /// A mode this fixture wants on the view — asserted host-side, so the
    /// `none` case is covered by every OTHER seam fixture and this covers the
    /// `some` one.
    fn view_mode() -> Option<String> {
        Some("agenda-guest-mode".to_string())
    }

    fn begin() -> u64 {
        FILES_SEEN.set(0);
        // OT.3b: this fixture's rows do not depend on the day or on any option,
        // so a constant is the honest generation — its results stay valid until
        // the files themselves change. A guest WITH scan-wide state (org's today
        // anchor, its keyword set) derives the key from that instead.
        1
    }

    fn scan(
        _path: String,
        text: String,
        tree: Option<&TreeSnapshot>,
    ) -> Result<Vec<Entry>, String> {
        // OT.3: text is always here; the tree comes beside it when the file's
        // extension resolves to a registered language. This fixture reports the
        // ROOT KIND when it got a tree — something no text scan could produce —
        // so the host test can tell the two apart.
        if let Some(snapshot) = tree {
            let root = snapshot.root();
            return Ok(vec![Entry {
                line: 0,
                end_line: 0,
                group: "tree".to_string(),
                label: format!("tree:{}:{}", root.kind(), root.named_child_count()),
                sort_key: 0,
            }]);
        }
        if text.contains("BROKEN") {
            return Err("agenda-guest: malformed file".to_string());
        }
        let seen = FILES_SEEN.get() + 1;
        FILES_SEEN.set(seen);
        let mut out = Vec::new();
        for (i, line) in text.lines().enumerate() {
            let Some(rest) = line.strip_prefix("* TODO ") else {
                continue;
            };
            // The row's key comes out of the file, so a host-side sort
            // assertion is testing the guest's data and not the walk order.
            let sort_key: i64 = rest.trim().parse().unwrap_or(0);
            out.push(Entry {
                line: i as u32,
                end_line: i as u32,
                group: format!("day-{sort_key}"),
                // The counter rides in the label so `begin`'s reset is
                // observable from the host without another export.
                label: format!("Day {sort_key} (file {seen})"),
                sort_key,
            });
        }
        Ok(out)
    }
}

export!(Component);
