//! LL.4: the transport and request lifecycle against a local TCP mock.

#![cfg(feature = "llm")]
mod llm_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use lattice_ai::llm::request::RETAINED_TERMINAL;
use lattice_ai::llm::{RequestRegistry, RequestState, StreamEvent};
use llm_support::*;
use tokio::io::AsyncWriteExt;

fn registry() -> RequestRegistry {
    RequestRegistry::new(tokio::runtime::Handle::current(), None, None)
}

const WAIT: Duration = Duration::from_secs(5);

#[tokio::test]
async fn streams_to_completion() {
    let body = format!("{SSE_HEAD}{OPENAI_OK_BODY}");
    let addr = serve_bytes(body.into_bytes()).await;
    let reg = registry();
    let rec = Recorder::default();
    let id = reg.start_with_key(
        target(),
        request(),
        provider_for(addr),
        key(),
        Box::new(rec.clone()),
    );
    assert_eq!(wait_terminal(&reg, id, WAIT).await, RequestState::Done);
    assert_eq!(rec.text(), "Héllo → 世界 ✓");
    assert!(
        rec.events
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, StreamEvent::Done(_))),
        "Done event delivered"
    );
}

#[tokio::test]
async fn request_carries_the_auth_header() {
    let seen = Arc::new(std::sync::Mutex::new(String::new()));
    let s2 = seen.clone();
    let addr = serve(move |req, mut s| {
        let s2 = s2.clone();
        async move {
            *s2.lock().unwrap() = req;
            let _ = s
                .write_all(format!("{SSE_HEAD}{OPENAI_OK_BODY}").as_bytes())
                .await;
        }
    })
    .await;
    let reg = registry();
    let id = reg.start_with_key(
        target(),
        request(),
        provider_for(addr),
        key(),
        Box::new(Recorder::default()),
    );
    wait_terminal(&reg, id, WAIT).await;
    assert!(
        seen.lock()
            .unwrap()
            .contains("Bearer sk-test-SECRET-0123456789")
    );
}

#[tokio::test]
async fn cancel_stops_a_stalled_stream() {
    // Headers and one event, then silence: the server never closes.
    let addr = serve(|_, mut s| async move {
        let _ = s
            .write_all(
                format!(
                    "{SSE_HEAD}data: {{\"choices\":[{{\"delta\":{{\"content\":\"a\"}}}}]}}\n\n"
                )
                .as_bytes(),
            )
            .await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    })
    .await;
    let reg = registry();
    let rec = Recorder::default();
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(rec.clone()),
    );
    // Wait until it is actually streaming and stalled.
    let t = Instant::now();
    while rec.text().is_empty() {
        assert!(t.elapsed() < WAIT, "never started streaming");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let cancelled_at = Instant::now();
    reg.cancel(id);
    let state = wait_terminal(&reg, id, Duration::from_secs(2)).await;
    assert_eq!(state, RequestState::Cancelled);
    assert!(
        cancelled_at.elapsed() < Duration::from_millis(100),
        "cancel took {:?}",
        cancelled_at.elapsed()
    );
}

#[tokio::test]
async fn cancel_while_the_server_never_answers() {
    // Accepts, reads, never replies: cancel must still be immediate.
    let addr = serve(|_, s| async move {
        let _held = s; // keep the socket open and silent
        tokio::time::sleep(Duration::from_secs(60)).await
    })
    .await;
    let reg = registry();
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(Recorder::default()),
    );
    tokio::time::sleep(Duration::from_millis(50)).await;
    let at = Instant::now();
    reg.cancel(id);
    assert_eq!(
        wait_terminal(&reg, id, Duration::from_secs(2)).await,
        RequestState::Cancelled
    );
    assert!(at.elapsed() < Duration::from_millis(100));
}

async fn failed_for(status_line: &str, body: &str) -> String {
    // `Retry-After: 0` so a transient status's retries do not wait.
    let raw = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nRetry-After: 0\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let addr = serve_bytes(raw.into_bytes()).await;
    let reg = registry();
    let id = reg.start_with_key(
        target(),
        request(),
        provider_for(addr),
        key(),
        Box::new(Recorder::default()),
    );
    failed_message(wait_terminal(&reg, id, WAIT).await)
}

#[tokio::test]
async fn http_401_is_a_named_failure_with_the_provider_message() {
    let m = failed_for(
        "401 Unauthorized",
        r#"{"error":{"message":"Invalid API key provided"}}"#,
    )
    .await;
    assert!(m.contains("authentication failed"), "{m}");
    assert!(m.contains("401"), "{m}");
    assert!(m.contains("Invalid API key provided"), "{m}");
}

#[tokio::test]
async fn http_429_is_rate_limited() {
    let m = failed_for(
        "429 Too Many Requests",
        r#"{"error":{"type":"rate_limit_error","message":"Slow down"}}"#,
    )
    .await;
    assert!(
        m.contains("rate limited") && m.contains("429") && m.contains("Slow down"),
        "{m}"
    );
}

fn refusal(status_line: &str, retry_after: &str) -> Vec<u8> {
    let body = r#"{"error":{"message":"Slow down"}}"#;
    format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nRetry-After: {retry_after}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// A server that refuses the first `refusals` requests with `status_line`
/// (asking to wait `retry_after`) and streams a reply after that. Returns
/// its address and a count of the requests it saw.
async fn refuses_then_answers(
    refusals: usize,
    status_line: &'static str,
    retry_after: &'static str,
) -> (std::net::SocketAddr, Arc<std::sync::atomic::AtomicUsize>) {
    use std::sync::atomic::Ordering;
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = seen.clone();
    let addr = serve(move |_, mut s| {
        let n = count.fetch_add(1, Ordering::SeqCst);
        async move {
            let bytes = if n < refusals {
                refusal(status_line, retry_after)
            } else {
                format!("{SSE_HEAD}{OPENAI_OK_BODY}").into_bytes()
            };
            let _ = s.write_all(&bytes).await;
            let _ = s.shutdown().await;
        }
    })
    .await;
    (addr, seen)
}

/// Rate limited or overloaded is "not now": the request is sent again, and
/// the reply that follows streams as if nothing happened. Meanwhile the
/// request shows as retrying, not silent.
#[tokio::test]
async fn a_transient_refusal_is_retried() {
    use std::sync::atomic::Ordering;
    for status in [
        "429 Too Many Requests",
        "529 Overloaded",
        "503 Service Unavailable",
    ] {
        let (addr, seen) = refuses_then_answers(2, status, "0").await;
        let reg = registry();
        let rec = Recorder::default();
        let id = reg.start_with_key(
            target(),
            request(),
            provider_for(addr),
            key(),
            Box::new(rec.clone()),
        );
        assert_eq!(
            wait_terminal(&reg, id, WAIT).await,
            RequestState::Done,
            "{status}"
        );
        assert_eq!(rec.text(), "Héllo → 世界 ✓", "{status}");
        assert_eq!(seen.load(Ordering::SeqCst), 3, "{status}: sent three times");
    }
}

/// After the last retry the failure is reported, saying it was retried.
#[tokio::test]
async fn a_refusal_that_persists_fails_after_its_retries() {
    use std::sync::atomic::Ordering;
    let (addr, seen) = refuses_then_answers(usize::MAX, "429 Too Many Requests", "0").await;
    let reg = registry();
    let id = reg.start_with_key(
        target(),
        request(),
        provider_for(addr),
        key(),
        Box::new(Recorder::default()),
    );
    let m = failed_message(wait_terminal(&reg, id, WAIT).await);
    assert!(m.contains("rate limited") && m.contains("Slow down"), "{m}");
    assert!(m.contains("after 3 attempts"), "{m}");
    assert_eq!(seen.load(Ordering::SeqCst), 3);
}

/// A provider asking for a longer wait than is worth sitting silent for is
/// not waited on: the failure names the wait, and nothing is sent again.
#[tokio::test]
async fn a_long_retry_after_is_reported_not_waited() {
    use std::sync::atomic::Ordering;
    let (addr, seen) = refuses_then_answers(usize::MAX, "429 Too Many Requests", "600").await;
    let reg = registry();
    let at = Instant::now();
    let id = reg.start_with_key(
        target(),
        request(),
        provider_for(addr),
        key(),
        Box::new(Recorder::default()),
    );
    let m = failed_message(wait_terminal(&reg, id, WAIT).await);
    assert!(m.contains("asks to wait 600s"), "{m}");
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert!(at.elapsed() < Duration::from_secs(2));
}

/// A request waiting to retry is shown as retrying, and stopping it ends
/// the wait at once.
#[tokio::test]
async fn a_retry_wait_shows_and_stops_at_once() {
    let (addr, _) = refuses_then_answers(usize::MAX, "529 Overloaded", "30").await;
    let reg = registry();
    let id = reg.start_with_key(
        target(),
        request(),
        provider_for(addr),
        key(),
        Box::new(Recorder::default()),
    );
    let t = Instant::now();
    loop {
        let state = info(&reg, id).map(|i| i.state);
        if let Some(RequestState::Retrying { attempt, reason }) = state {
            assert_eq!(attempt, 1);
            assert_eq!(reason, "provider overloaded");
            break;
        }
        assert!(t.elapsed() < WAIT, "never showed as retrying: {state:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let at = Instant::now();
    reg.cancel(id);
    assert_eq!(
        wait_terminal(&reg, id, Duration::from_secs(2)).await,
        RequestState::Cancelled
    );
    assert!(
        at.elapsed() < Duration::from_millis(200),
        "{:?}",
        at.elapsed()
    );
}

#[tokio::test]
async fn http_500_is_a_provider_error() {
    let m = failed_for("500 Internal Server Error", "boom").await;
    assert!(
        m.contains("provider error") && m.contains("500") && m.contains("boom"),
        "{m}"
    );
}

#[tokio::test]
async fn a_long_error_body_is_bounded() {
    let body = "x".repeat(100_000);
    let m = failed_for("500 Internal Server Error", &body).await;
    assert!(
        m.chars().count() < 400,
        "excerpt must be bounded, got {}",
        m.len()
    );
}

#[tokio::test]
async fn connection_drop_mid_stream_fails_not_hangs() {
    // Promise a chunked body, send one event, then drop the socket abruptly.
    let addr = serve(|_, mut s| async move {
        let ev = "data: {\"choices\":[{\"delta\":{\"content\":\"partial\"}}]}\n\n";
        let _ = s
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{ev}\r\n",
                    ev.len()
                )
                .as_bytes(),
            )
            .await;
        drop(s);
    })
    .await;
    let reg = registry();
    let rec = Recorder::default();
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(rec.clone()),
    );
    let m = failed_message(wait_terminal(&reg, id, WAIT).await);
    assert!(!m.is_empty());
    assert_eq!(
        rec.text(),
        "partial",
        "events before the drop were delivered"
    );
}

#[tokio::test]
async fn clean_eof_before_completion_is_a_truncation_failure() {
    let body = "data: {\"choices\":[{\"delta\":{\"content\":\"half\"}}]}\n\n";
    let addr = serve_bytes(format!("{SSE_HEAD}{body}").into_bytes()).await;
    let reg = registry();
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(Recorder::default()),
    );
    let m = failed_message(wait_terminal(&reg, id, WAIT).await);
    assert!(m.contains("before the response completed"), "{m}");
}

#[tokio::test]
async fn a_non_event_stream_200_is_a_failure_not_a_silent_empty_reply() {
    let body = "<html><body>Captive portal login</body></html>";
    let raw = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let addr = serve_bytes(raw.into_bytes()).await;
    let reg = registry();
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(Recorder::default()),
    );
    let m = failed_message(wait_terminal(&reg, id, WAIT).await);
    assert!(
        m.contains("not an event stream") && m.contains("text/html"),
        "{m}"
    );
}

#[tokio::test]
async fn an_idle_connection_is_abandoned() {
    let addr = serve(|_, mut s| async move {
        let _ = s.write_all(SSE_HEAD.as_bytes()).await;
        tokio::time::sleep(Duration::from_secs(60)).await;
    })
    .await;
    let reg = RequestRegistry::with_idle_timeout(
        tokio::runtime::Handle::current(),
        None,
        None,
        Duration::from_millis(100),
    );
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(Recorder::default()),
    );
    let m = failed_message(wait_terminal(&reg, id, WAIT).await);
    assert!(m.contains("no data"), "{m}");
}

#[tokio::test]
async fn connection_refused_is_a_failure() {
    // Bind then drop to get a port nothing listens on.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let reg = registry();
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(Recorder::default()),
    );
    let m = failed_message(wait_terminal(&reg, id, WAIT).await);
    assert!(m.contains("could not connect"), "{m}");
}

#[tokio::test]
async fn a_sink_stop_ends_the_request() {
    let body = format!("{SSE_HEAD}{OPENAI_OK_BODY}");
    let addr = serve_bytes(body.into_bytes()).await;
    let reg = registry();
    let rec = Recorder {
        stop_after: Some(1),
        ..Default::default()
    };
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(rec.clone()),
    );
    assert_eq!(wait_terminal(&reg, id, WAIT).await, RequestState::Cancelled);
    assert_eq!(
        rec.events.lock().unwrap().len(),
        1,
        "no event delivered after Stop"
    );
}

#[tokio::test]
async fn terminal_requests_are_retained_to_a_bound() {
    let body = format!("{SSE_HEAD}{OPENAI_OK_BODY}");
    let addr = serve_bytes(body.into_bytes()).await;
    let reg = registry();
    let mut ids = Vec::new();
    for _ in 0..RETAINED_TERMINAL + 4 {
        let id = reg.start(
            target(),
            request(),
            provider_for(addr),
            Box::new(Recorder::default()),
        );
        wait_terminal(&reg, id, WAIT).await;
        ids.push(id);
    }
    let snap = reg.snapshot();
    assert_eq!(snap.len(), RETAINED_TERMINAL);
    assert!(snap.iter().all(|i| i.state.is_terminal()));
    assert_eq!(
        snap.last().map(|i| i.id),
        ids.last().copied(),
        "most recent kept"
    );
    assert!(snap.iter().all(|i| i.id != ids[0]), "oldest evicted");
}

#[tokio::test]
async fn a_live_request_is_never_evicted() {
    let stall = serve(|_, s| async move {
        let _held = s; // keep the socket open and silent
        tokio::time::sleep(Duration::from_secs(60)).await
    })
    .await;
    let ok = serve_bytes(format!("{SSE_HEAD}{OPENAI_OK_BODY}").into_bytes()).await;
    let reg = registry();
    let live = reg.start(
        target(),
        request(),
        provider_for(stall),
        Box::new(Recorder::default()),
    );
    for _ in 0..RETAINED_TERMINAL + 3 {
        let id = reg.start(
            target(),
            request(),
            provider_for(ok),
            Box::new(Recorder::default()),
        );
        wait_terminal(&reg, id, WAIT).await;
    }
    assert!(
        !info(&reg, live)
            .expect("live request retained")
            .state
            .is_terminal()
    );
    // Once cancelled it is just the oldest terminal entry and may be evicted.
    reg.cancel(live);
    let t = Instant::now();
    while info(&reg, live).is_some() {
        assert!(t.elapsed() < WAIT, "cancelled request never aged out");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn a_terminal_state_is_final() {
    let addr = serve_bytes(format!("{SSE_HEAD}{OPENAI_OK_BODY}").into_bytes()).await;
    let reg = registry();
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(Recorder::default()),
    );
    wait_terminal(&reg, id, WAIT).await;
    reg.cancel(id); // no-op once terminal
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(info(&reg, id).unwrap().state, RequestState::Done);
}

#[tokio::test]
async fn a_panicking_sink_still_ends_in_failed() {
    struct Boom;
    impl lattice_ai::llm::Sink for Boom {
        fn event(&mut self, _: StreamEvent) -> lattice_ai::llm::SinkFlow {
            panic!("sink exploded");
        }
    }
    let addr = serve_bytes(format!("{SSE_HEAD}{OPENAI_OK_BODY}").into_bytes()).await;
    let reg = registry();
    let id = reg.start(target(), request(), provider_for(addr), Box::new(Boom));
    let m = failed_message(wait_terminal(&reg, id, WAIT).await);
    assert!(m.contains("panicked"), "{m}");
}

/// Throughput: a large stream arrives intact. Responsiveness: while it decodes,
/// the runtime the test runs on keeps ticking promptly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_large_stream_is_delivered_while_the_runtime_stays_responsive() {
    let mut body = String::from(SSE_HEAD);
    for _ in 0..20_000 {
        body.push_str("data: {\"choices\":[{\"delta\":{\"content\":\"tok \"}}]}\n\n");
    }
    body.push_str("data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n");
    let addr = serve_bytes(body.into_bytes()).await;
    let reg = registry();
    let rec = Recorder::default();
    let id = reg.start(
        target(),
        request(),
        provider_for(addr),
        Box::new(rec.clone()),
    );
    let mut worst = Duration::ZERO;
    let start = Instant::now();
    loop {
        let t = Instant::now();
        tokio::time::sleep(Duration::from_millis(2)).await;
        worst = worst.max(t.elapsed());
        if info(&reg, id).is_some_and(|i| i.state.is_terminal()) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "stream too slow");
    }
    assert_eq!(info(&reg, id).unwrap().state, RequestState::Done);
    assert_eq!(rec.text().len(), 20_000 * 4);
    assert!(
        worst < Duration::from_millis(250),
        "runtime stalled for {worst:?}"
    );
}
