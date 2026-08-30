//! PH7.4c.1b picker fixture guest.
//!
//! A minimal `wasm32-wasip2` component implementing the `picker-source-plugin`
//! world. It exists to drive, through a real guest, the per-plugin actor bridge
//! (`PickerActor` + `PickerClient`, `picker_task.rs`):
//!   - `spec()` returns fixed metadata (proves the no-`result` reply path).
//!   - `init(ctx, args)` returns two candidate pairs that ECHO the inputs — one
//!     the joined `args`, one the projected `ctx.workspace_root` — so the host
//!     can assert the inputs crossed. `args` containing `"fail"` returns the WIT
//!     typed `err` (proves the guest-error path is distinct from a host trap).
//!   - `accept(ctx, routing)` maps the routing token it emitted in `init` to a
//!     `picker-accept-outcome`; an unexpected token is a WIT `err`.
//!
//! It imports `host-services` (per the world) but does not call `walk` — the
//! guest→host call rides the real `fuzzy-finder` consumer (PH7.4d).

wit_bindgen::generate!({
    world: "picker-source-plugin",
    path: "../../../../../wit",
});

use exports::lattice::plugin_host::picker_source::{CandidatePair, Guest as PickerSourceGuest};
use lattice::plugin_host::picker_registry::register_picker_source;
use lattice::plugin_host::types::{
    CandidateData, CandidateKind, PickerAcceptOutcome, PickerContext, PickerSourceSpec,
    RawCandidate, RoutingPayload,
};

/// OR.5b: this fixture registers TWO sources, because one is exactly the case
/// the old seam already handled and the second is the whole slice.
const FIXTURE: &str = "fixture";
const SECOND: &str = "fixture-second";

struct Component;

/// OR.5b: the WORLD-level registration export. The host calls it once; the
/// guest declares each source through the imported `register-picker-source`.
///
/// Two sources, because one is exactly the case the old seam already handled
/// and the second is the whole point of the slice.
impl Guest for Component {
    fn register_picker_sources() {
        register_picker_source(&PickerSourceSpec {
            id: FIXTURE.to_string(),
            doc: "PH7.4c.1b fixture picker source".to_string(),
            args_schema: Vec::new(),
            args_hint: "[fail]".to_string(),
            live: false,
            // OR.5: the source declares that it can create what the query
            // names. `%s` is replaced by the query when the row renders.
            create_label: Some("Create fixture: %s".to_string()),
        });
        register_picker_source(&PickerSourceSpec {
            id: SECOND.to_string(),
            doc: "OR.5b: a SECOND source from the same component".to_string(),
            args_schema: Vec::new(),
            args_hint: String::new(),
            live: false,
            create_label: None,
        });
    }
}

impl PickerSourceGuest for Component {
    fn init(source: String, ctx: PickerContext, args: Vec<String>) -> Result<Vec<CandidatePair>, String> {
        // OR.5b: the second source answers differently, so a test can prove the
        // id ROUTED rather than that two registrations both reached one body.
        if source == SECOND {
            return Ok(vec![CandidatePair {
                candidate: candidate("from-second"),
                routing: RoutingPayload::OpenFile("/second".to_string()),
            }]);
        }
        // The guest's typed WIT `result` err path — distinct from a host trap.
        if args.iter().any(|a| a == "fail") {
            return Err("fixture asked to fail".to_string());
        }
        // Two candidates that echo the inputs back: one the joined args (proves
        // `args` crossed), one the projected workspace root (proves the
        // `PickerContext` projection crossed). Each carries a routing token the
        // guest consumes in `accept`.
        Ok(vec![
            CandidatePair {
                candidate: candidate(&args.join(",")),
                routing: RoutingPayload::OpenFile(format!("/args/{}", args.join("/"))),
            },
            CandidatePair {
                candidate: candidate(&ctx.workspace_root),
                routing: RoutingPayload::Buffer(0),
            },
        ])
    }

    fn accept(
        source: String,
        _ctx: PickerContext,
        routing: RoutingPayload,
    ) -> Result<PickerAcceptOutcome, String> {
        // OR.5b: the second source's accept is distinguishable too — otherwise a
        // test could not tell "routed to the right source" from "there is only
        // one body".
        if source == SECOND {
            return Ok(PickerAcceptOutcome::OpenFile("/second/accepted".to_string()));
        }
        match routing {
            RoutingPayload::OpenFile(p) => Ok(PickerAcceptOutcome::OpenFile(p)),
            RoutingPayload::Buffer(id) => Ok(PickerAcceptOutcome::SwitchBuffer(id)),
            // OR.5: the create row. The query crosses VERBATIM — the host must
            // not have trimmed, lowercased or otherwise had an opinion about a
            // namespace it does not own — so the fixture echoes it back inside
            // a path the test can compare exactly.
            RoutingPayload::Create(query) => {
                Ok(PickerAcceptOutcome::OpenFile(format!("/created/{query}")))
            }
            _ => Err("fixture: unexpected routing token".to_string()),
        }
    }
}

/// A plain candidate whose `text`/`display` echo `text`, source tagged
/// `"fixture"`. Minimal but a valid `RawCandidate` the host round-trips.
fn candidate(text: &str) -> RawCandidate {
    RawCandidate {
        insert_text: None,
        text: text.to_string(),
        display: text.to_string(),
        source: Some("fixture".to_string()),
        kind: CandidateKind::Plain,
        data: CandidateData::Plain,
        annotations: Vec::new(),
    }
}

export!(Component);
