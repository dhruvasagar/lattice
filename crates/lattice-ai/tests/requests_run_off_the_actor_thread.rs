//! A request started from a `current_thread` runtime (the editor actor's
//! config) must run on the registry's own runtime, not the caller's thread.

#![cfg(feature = "llm")]
mod llm_support;

use std::time::Duration;

use lattice_ai::llm::{RequestRegistry, RequestState};
use llm_support::*;

#[tokio::test(flavor = "current_thread")]
async fn requests_run_off_the_actor_thread() {
    let caller = std::thread::current().id();
    let io_rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let reg = RequestRegistry::new(io_rt.handle().clone(), None, None);

    let addr = serve_bytes(format!("{SSE_HEAD}{OPENAI_OK_BODY}").into_bytes()).await;
    let rec = Recorder::default();
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(rec.clone()),
    );

    // `start` returned without running anything on this thread; the awaits
    // below drive only the mock server.
    let state = wait_terminal(&reg, id, Duration::from_secs(5)).await;
    assert_eq!(state, RequestState::Done);

    let threads = rec.threads.lock().unwrap();
    assert!(!threads.is_empty(), "sink was called");
    assert!(
        threads.iter().all(|t| *t != caller),
        "the sink ran on the caller's (actor) thread"
    );
    drop(threads);
    io_rt.shutdown_background();
}
