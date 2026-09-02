//! OM.A1 agenda fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the
//! `scanned-excerpt-source-plugin` world, driving the agenda producer actor
//! (`scan_task.rs`) through real host→guest calls:
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
    world: "scanned-excerpt-source-plugin",
    path: "../../../../../wit",
});

// Per-scan state. Single-threaded guest with its calls serialised by the
// host's per-plugin actor, so a `thread_local` is the whole story — and this
// is exactly the state `begin` exists to clear.
thread_local! {
    static FILES_SEEN: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// OA.11a: what THIS scan was opened for. Stashed in `begin` and read in
    /// `scan`, which is the ordering the seam promises and the shape a real
    /// source uses (org resolves its custom command here).
    static SCAN_ARGS: std::cell::RefCell<Vec<String>> = const {
        std::cell::RefCell::new(Vec::new())
    };
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

    /// AF.1: two paths that could only have come from the guest.
    ///
    /// Deliberately fixed and obviously synthetic. The question this fixture
    /// answers is "does what the guest said reach the host, verbatim" — the
    /// failure OT.4 spent a slice on was a seam that looked wired and delivered
    /// nothing, and a fixture returning the empty list every real source also
    /// returns cannot tell the two apart.
    ///
    /// `~` is left unexpanded on purpose: expansion is the host's job and the
    /// second entry is what proves it happens on the far side.
    fn roots() -> Vec<String> {
        // AF.3: read from an OPTION when one is set, so the test can tell a
        // wired config path from an unwired one. The agenda store was the only
        // seam store never handed the registry, and every agenda test drove
        // `extensions` / `begin` / `scan` — none of which reads an option — so
        // the gap was invisible until `roots` needed it.
        if let Some(v) = crate::lattice::plugin_host::config::get_option("roots") {
            if !v.trim().is_empty() {
                return v.lines().map(str::trim).map(str::to_string).collect();
            }
        }
        vec![
            "/agenda-guest/notes".to_string(),
            "~/agenda-guest/one.org".to_string(),
        ]
    }

    /// OA.22: echo the args back, so a host test can assert the label crossed
    /// the seam without this fixture growing a view model to describe.
    fn describe(args: Vec<String>) -> String {
        args.join(" · ")
    }

    fn begin(args: Vec<String>) -> u64 {
        FILES_SEEN.set(0);
        // OA.11a: the args are per-scan state like any other, so they are
        // captured here — the one call the seam guarantees runs before `roots`
        // and every `scan`.
        SCAN_ARGS.with(|a| *a.borrow_mut() = args.clone());
        // OT.3b: this fixture's rows do not otherwise depend on the day or on
        // any option, so the generation is a constant — its results stay valid
        // until the files themselves change. A guest WITH scan-wide state
        // (org's today anchor, its keyword set) derives the key from that.
        //
        // OA.11a folds the args in, and that is not decoration: a scan asked
        // for a different thing must not be served rows the previous one
        // cached. A guest that took args and did NOT key on them would render
        // the first command's rows under the second command's name.
        let mut key: u64 = 1;
        for a in &args {
            for b in a.as_bytes() {
                key = key.wrapping_mul(31).wrapping_add(*b as u64);
            }
            key = key.wrapping_mul(31);
        }
        key
    }

    fn scan(
        _path: String,
        text: String,
        tree: Option<&TreeSnapshot>,
    ) -> Result<ScanResult, String> {
        // OA.14b: one deterministic, obviously-synthetic span, so a host test
        // can tell "the clock half of the record crossed" from "the guest
        // happened to have nothing to say". Independent of the rows below —
        // which is the property the seam exists for.
        let clock = vec![ClockSpan {
            line: 0,
            outline: vec!["Clocked".to_string()],
            day: 20_000,
            minutes: 90,
        }];
        // OT.3: text is always here; the tree comes beside it when the file's
        // extension resolves to a registered language. This fixture reports the
        // ROOT KIND when it got a tree — something no text scan could produce —
        // so the host test can tell the two apart.
        if let Some(snapshot) = tree {
            let root = snapshot.root();
            return Ok(ScanResult {
                entries: vec![Entry {
                    line: 0,
                    end_line: 0,
                    group: "tree".to_string(),
                    label: format!("tree:{}:{}", root.kind(), root.named_child_count()),
                    sort_key: 0,
                    spans: Vec::new(),
                }],
                clock,
            });
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
                // observable from the host without another export, and OA.11a's
                // scan args ride beside it for the same reason — a seam that
                // looks wired and delivers nothing is the failure this fixture
                // exists to catch.
                label: format!(
                    "Day {sort_key} (file {seen}){}",
                    SCAN_ARGS.with(|a| {
                        let a = a.borrow();
                        if a.is_empty() {
                            String::new()
                        } else {
                            format!(" [{}]", a.join(","))
                        }
                    })
                ),
                sort_key,
                // OA.5: one styled run over the row's keyword, so the host
                // test can assert the spans crossed the boundary at all.
                // Offsets are into THIS row's line, which is the contract.
                spans: vec![crate::lattice::plugin_host::types::DisplaySpan {
                    start: 2,
                    end: 6,
                    slot: "keyword".to_string(),
                }],
            });
        }
        Ok(ScanResult {
            entries: out,
            clock,
        })
    }
}

export!(Component);
