//! WebSocket accept + authorization handshake.
//!
//! Upgrades an accepted TCP stream to a WebSocket, gating on the
//! `x-claude-code-ide-authorization` header. A mismatched token is
//! rejected during the handshake with HTTP 401 — the connection never
//! reaches the MCP loop. Loopback bind + this token are the security
//! boundary (design §4).

use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;

use crate::auth;
use crate::error::Result;

/// Accept a WebSocket connection on `stream`, requiring the
/// `x-claude-code-ide-authorization` header to match `expected_token`
/// (constant-time). Rejects with HTTP 401 otherwise — the returned future
/// resolves to an error and the connection is dropped.
// The handshake callback's `Result<Response, ErrorResponse>` return type is
// dictated by tokio-tungstenite's `Callback` trait; `ErrorResponse`
// (`http::Response<Option<String>>`) is inherently large, so we can't shrink
// the Err variant here (clippy `result_large_err`).
#[allow(clippy::result_large_err)]
pub async fn accept(
    stream: TcpStream,
    expected_token: &str,
) -> Result<WebSocketStream<TcpStream>> {
    let expected = expected_token.to_string();
    let callback = move |request: &Request,
                         response: Response|
          -> std::result::Result<Response, ErrorResponse> {
        let provided = request
            .headers()
            .get(auth::AUTH_HEADER)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if auth::header_matches(&expected, provided) {
            Ok(response)
        } else {
            let mut err = ErrorResponse::new(Some("authorization rejected".to_string()));
            *err.status_mut() = StatusCode::UNAUTHORIZED;
            Err(err)
        }
    };
    let ws = tokio_tungstenite::accept_hdr_async(stream, callback).await?;
    Ok(ws)
}
