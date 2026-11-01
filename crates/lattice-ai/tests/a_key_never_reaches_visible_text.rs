//! Review focus 4: a key must not reach visible text through an error -- not via
//! a provider echoing the header, nor a proxy's HTML error page, nor the URL.

mod llm_support;

use std::time::Duration;

use lattice_ai::llm::transport::scrub;
use lattice_ai::llm::{ApiKey, RequestRegistry, RequestState};
use llm_support::*;
use tokio::io::AsyncWriteExt;

const KEY: &str = "sk-test-SECRET-0123456789";

fn registry() -> RequestRegistry {
    RequestRegistry::new(tokio::runtime::Handle::current(), None, None)
}

fn assert_clean(msg: &str) {
    assert!(!msg.contains(KEY), "key leaked: {msg}");
    assert!(!msg.contains("SECRET-0123"), "key fragment leaked: {msg}");
}

async fn run_to_failure(addr: std::net::SocketAddr) -> String {
    let reg = registry();
    let id = reg.start_with_key(
        target(),
        request(),
        provider_for(addr),
        key(),
        Box::new(Recorder::default()),
    );
    let state = wait_terminal(&reg, id, Duration::from_secs(5)).await;
    let msg = failed_message(state.clone());
    // Everything visible through the snapshot is clean too.
    for i in reg.snapshot().iter() {
        assert_clean(&format!("{:?}", i));
    }
    msg
}

#[tokio::test]
async fn a_server_echoing_the_request_headers_in_its_error_is_scrubbed() {
    // Echo the whole request (headers included) inside a JSON error message.
    let addr = serve(|req, mut s| async move {
        let body = serde_json::json!({"error": {"message": format!("bad request: {req}")}}).to_string();
        let raw = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(raw.as_bytes()).await;
    })
    .await;
    let msg = run_to_failure(addr).await;
    assert_clean(&msg);
    assert!(
        msg.contains("bad request"),
        "the message itself survives: {msg}"
    );
    assert!(!msg.to_lowercase().contains("bearer sk-"), "{msg}");
}

#[tokio::test]
async fn a_proxy_html_page_quoting_the_header_is_scrubbed_and_bounded() {
    let addr = serve(|req, mut s| async move {
        let auth = req
            .lines()
            .find(|l| l.to_lowercase().starts_with("authorization:"))
            .unwrap_or("")
            .to_string();
        let body = format!(
            "<html><head><title>502 Bad Gateway</title></head><body><h1>Proxy error</h1>\n<pre>\n{auth}\n</pre>{}</body></html>",
            "<p>filler</p>".repeat(500)
        );
        let raw = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(raw.as_bytes()).await;
    })
    .await;
    let msg = run_to_failure(addr).await;
    assert_clean(&msg);
    assert!(msg.contains("502"), "{msg}");
    assert!(msg.chars().count() < 400, "bounded: {}", msg.len());
    assert!(!msg.contains('\n'), "single line");
}

#[tokio::test]
async fn an_event_stream_error_carrying_the_key_is_scrubbed() {
    let addr = serve(|_, mut s| async move {
        let ev = serde_json::json!({"error": {"message": format!("key {KEY} is revoked"), "type": format!("auth-{KEY}")}});
        let _ = s
            .write_all(format!("{SSE_HEAD}data: {ev}\n\n").as_bytes())
            .await;
    })
    .await;
    let reg = registry();
    let rec = Recorder::default();
    let id = reg.start_with_key(
        target(),
        request(),
        provider_for(addr),
        key(),
        Box::new(rec.clone()),
    );
    let msg = failed_message(wait_terminal(&reg, id, Duration::from_secs(5)).await);
    assert_clean(&msg);
    // What the sink (and so the UI) received is clean too.
    let seen = format!("{:?}", rec.events.lock().unwrap());
    assert!(
        seen.contains("revoked"),
        "the error still reached the sink: {seen}"
    );
    assert_clean(&seen);
}

#[tokio::test]
async fn a_redirect_is_not_followed_and_never_receives_the_key() {
    let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let h2 = hits.clone();
    let target_addr = serve(move |req, _s| {
        let h2 = h2.clone();
        async move { h2.lock().unwrap().push(req) }
    })
    .await;
    let redirector = serve(move |_, mut s| async move {
        let raw = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{target_addr}/steal?k=1\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let _ = s.write_all(raw.as_bytes()).await;
    })
    .await;
    let reg = registry();
    let id = reg.start_with_key(
        target(),
        request(),
        provider_for(redirector),
        key(),
        Box::new(Recorder::default()),
    );
    let msg = failed_message(wait_terminal(&reg, id, Duration::from_secs(5)).await);
    assert!(
        msg.contains("redirected") && msg.contains(&target_addr.to_string()),
        "{msg}"
    );
    assert!(!msg.contains("steal"), "path not echoed: {msg}");
    assert_clean(&msg);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        hits.lock().unwrap().is_empty(),
        "redirect target was contacted"
    );
}

#[tokio::test]
async fn a_200_json_body_echoing_the_key_is_scrubbed() {
    let addr = serve(|req, mut s| async move {
        let body = format!("{{\"echo\":\"{}\"}}", req.replace('\r', "").replace('\n', " "));
        let raw = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = s.write_all(raw.as_bytes()).await;
    })
    .await;
    let msg = run_to_failure(addr).await;
    assert_clean(&msg);
}

#[test]
fn scrub_removes_the_key_and_auth_shaped_values() {
    let k = ApiKey::new(KEY);
    let out = scrub(
        &format!("a {KEY} b Authorization: Bearer abc.def-123 c BEARER xyz"),
        &k,
    );
    assert!(
        !out.contains(KEY) && !out.contains("abc.def") && !out.contains("xyz"),
        "{out}"
    );
    assert!(
        out.contains("a ") && out.contains(" c "),
        "surrounding text kept: {out}"
    );
    // Empty key never turns `replace("")` into garbage.
    assert_eq!(scrub("plain text", &ApiKey::new("")), "plain text");
    // Not a scheme: "bearer" inside a word is left alone.
    assert_eq!(
        scrub("the strongbearer thing", &k),
        "the strongbearer thing"
    );
}

#[tokio::test]
async fn a_connection_error_url_carrying_the_key_is_scrubbed() {
    // A key in the URL (query-string auth) must not survive in a connect error.
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    drop(l);
    let p = std::sync::Arc::new(lattice_ai::llm::OpenAiProvider::new(format!(
        "http://{addr}/v1?key={KEY}"
    )));
    let reg = registry();
    let id = reg.start_with_key(target(), request(), p, key(), Box::new(Recorder::default()));
    let state = wait_terminal(&reg, id, Duration::from_secs(5)).await;
    assert!(matches!(state, RequestState::Failed(_)));
    assert_clean(&failed_message(state));
}
