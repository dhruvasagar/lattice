//! The native web tools (AT.2, design `llm-assistant-tools.md` §3): `search`
//! and `fetch`, served as the `web` source beside lattice's own.
//!
//! **`web.search` runs; `web.fetch` asks, per host.** A search goes to the
//! backend the user configured, with a query the transcript shows. A fetch can
//! go anywhere, and a model holding the project's files (the read tools) and
//! untrusted text (any result) could be talked by a page into fetching
//! `evil.example/?q=<your notes>`. The question is the gate, and it names the
//! host, which is what the user decides on; "allow for this chat" covers that
//! host only ([`ChatAllow::Host`]).
//!
//! **A fetch reaches public addresses only.** A host that is loopback,
//! private, link-local or otherwise not on the public internet is refused,
//! both as a literal in the URL and as what a name resolves to: the fetch
//! client resolves through [`PublicOnly`], so a name whose answer changes
//! between a check and the connect still cannot reach the user's network.
//! Behind a configured HTTP proxy the proxy resolves names; the literal check
//! still applies there.
//!
//! **Redirects are not followed across hosts.** A redirect within the host is
//! followed (a few hops); one to another host is reported, so following it is
//! a new fetch, asked about like any other.
//!
//! Search requests go through a client of their own, without the address
//! filter: the backend is the user's choice, and a SearXNG instance is often
//! on localhost.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use reqwest::Url;
use reqwest::header::{ACCEPT, CONTENT_TYPE, LOCATION};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{BoxFuture, CallContext, ChatAllow, SourceTool, ToolError, ToolOutput, ToolSource};
use crate::conversation::ToolKind;
use crate::llm::ToolDescriptor;
use crate::llm::config::{SearchBackend, WebConfig, resolve_key_source};

/// The most of a body a fetch reads.
pub const MAX_BODY: usize = 2 * 1024 * 1024;
/// The most text one fetch returns; `offset` reads on.
pub const MAX_TEXT: usize = 40_000;
/// Results a search returns when the call names no count, and the most.
const SEARCH_COUNT: usize = 8;
const MAX_SEARCH_COUNT: usize = 20;
/// Redirects followed within one host.
const MAX_HOPS: usize = 5;
/// A whole fetch or search, connect to last byte.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

const BRAVE_URL: &str = "https://api.search.brave.com/res/v1/web/search";

/// The `[llm.web]` table as it is now, asked per call so a reload applies.
pub type WebConfigFn = Arc<dyn Fn() -> WebConfig + Send + Sync>;

/// `web.search` and `web.fetch`.
pub struct WebTools {
    config: WebConfigFn,
    search: reqwest::Client,
    fetch: reqwest::Client,
    /// Off only in this module's tests, whose server is on loopback.
    public_only: bool,
}

impl WebTools {
    pub fn new(config: WebConfigFn) -> Result<Self, String> {
        Self::build(config, true)
    }

    fn build(config: WebConfigFn, public_only: bool) -> Result<Self, String> {
        let base = || {
            reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(CONNECT_TIMEOUT)
                .user_agent(concat!("lattice/", env!("CARGO_PKG_VERSION")))
        };
        let search = base()
            .build()
            .map_err(|e| format!("could not create the HTTP client: {e}"))?;
        let mut fetch = base();
        if public_only {
            fetch = fetch.dns_resolver(Arc::new(PublicOnly));
        }
        let fetch = fetch
            .build()
            .map_err(|e| format!("could not create the HTTP client: {e}"))?;
        Ok(Self {
            config,
            search,
            fetch,
            public_only,
        })
    }

    fn catalogue(&self) -> Vec<SourceTool> {
        let mut tools = Vec::new();
        if (self.config)().search.is_some() {
            tools.push(SourceTool {
                descriptor: ToolDescriptor {
                    name: "search".into(),
                    description: "Search the web. Returns the title, URL and a snippet of each \
                                  result. Use fetch to read a result."
                        .into(),
                    input_schema: json!({
                        "type": "object",
                        "properties": {
                            "query": {"type": "string", "description": "What to search for"},
                            "count": {"type": "integer", "minimum": 1, "maximum": MAX_SEARCH_COUNT,
                                      "description": "How many results, default 8"}
                        },
                        "required": ["query"]
                    }),
                },
                kind: ToolKind::Search,
            });
        }
        tools.push(SourceTool {
            descriptor: ToolDescriptor {
                name: "fetch".into(),
                description: format!(
                    "Read a web page as markdown, links included. The user is asked before a \
                     new site is read and may refuse. Returns up to {MAX_TEXT} characters; pass \
                     offset to read further."
                ),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "An http or https URL"},
                        "offset": {"type": "integer", "minimum": 0,
                                   "description": "Character to start from, for a long page"}
                    },
                    "required": ["url"]
                }),
            },
            // Not `Read`: a fetch must not join the group that runs unasked.
            kind: ToolKind::Other,
        });
        tools
    }

    async fn search(&self, input: Value) -> Result<ToolOutput, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            query: String,
            count: Option<usize>,
        }
        let args: Args = parse(input)?;
        if args.query.trim().is_empty() {
            return Err(ToolError::InvalidArguments("query is empty".into()));
        }
        let count = args
            .count
            .unwrap_or(SEARCH_COUNT)
            .clamp(1, MAX_SEARCH_COUNT);
        let Some(backend) = (self.config)().search else {
            return Err(ToolError::Refused(
                "no web search backend is configured ([llm.web] search)".into(),
            ));
        };
        let (request, key) = match &backend {
            SearchBackend::Brave { key } => {
                let key = resolve_key_source("web search brave", key)
                    .await
                    .map_err(ToolError::Failed)?;
                let url = with_query(
                    BRAVE_URL,
                    &[("q", &args.query), ("count", &count.to_string())],
                )?;
                let request = self
                    .search
                    .get(url)
                    .header(ACCEPT, "application/json")
                    .header("X-Subscription-Token", key.expose());
                (request, Some(key))
            }
            SearchBackend::Searxng { base_url } => {
                let url = with_query(
                    &format!("{base_url}/search"),
                    &[("q", &args.query), ("format", "json")],
                )?;
                let request = self.search.get(url).header(ACCEPT, "application/json");
                (request, None)
            }
        };
        let scrub = |text: String| match &key {
            Some(k) => crate::llm::transport::scrub(&text, k),
            None => text,
        };
        let response = request
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|e| ToolError::Failed(scrub(format!("web search failed: {e}"))))?;
        let status = response.status();
        let body = read_capped(response, MAX_BODY)
            .await
            .map_err(|e| ToolError::Failed(scrub(e)))?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&body.bytes);
            return Err(ToolError::Failed(scrub(format!(
                "web search ({}) answered {status}: {}",
                backend.name(),
                first_line(&text)
            ))));
        }
        let hits = match backend {
            SearchBackend::Brave { .. } => decode_brave(&body.bytes),
            SearchBackend::Searxng { .. } => decode_searxng(&body.bytes),
        }
        .map_err(|e| ToolError::Failed(scrub(e)))?;
        Ok(ToolOutput {
            text: format_hits(&args.query, &hits, count),
        })
    }

    async fn fetch(&self, input: Value) -> Result<ToolOutput, ToolError> {
        #[derive(Deserialize)]
        struct Args {
            url: String,
            #[serde(default)]
            offset: usize,
        }
        let args: Args = parse(input)?;
        let mut url = fetchable(&args.url)?;
        if self.public_only {
            check_literal_host(&url)?;
        }
        let first_host = host_key(&url);
        let mut hops = 0;
        let response = loop {
            let response = self
                .fetch
                .get(url.clone())
                .header(
                    ACCEPT,
                    "text/html, text/markdown, text/plain, application/json, */*;q=0.5",
                )
                .timeout(REQUEST_TIMEOUT)
                .send()
                .await
                .map_err(|e| ToolError::Failed(fetch_error(&url, &e)))?;
            if !response.status().is_redirection() {
                break response;
            }
            let Some(next) = response
                .headers()
                .get(LOCATION)
                .and_then(|l| l.to_str().ok())
                .and_then(|l| url.join(l).ok())
            else {
                return Err(ToolError::Failed(format!(
                    "{url} answered {} with no usable Location",
                    response.status()
                )));
            };
            if host_key(&next) != first_host {
                return Ok(ToolOutput {
                    text: format!(
                        "{url} redirects to {next}, on another site. Fetch that URL to follow \
                         it; the user is asked first."
                    ),
                });
            }
            hops += 1;
            if hops > MAX_HOPS {
                return Err(ToolError::Failed(format!(
                    "{} redirected more than {MAX_HOPS} times",
                    args.url
                )));
            }
            url = next;
        };
        let status = response.status();
        if !status.is_success() {
            return Err(ToolError::Failed(format!("{url} answered {status}")));
        }
        let mime = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.split(';').next().unwrap_or(v).trim().to_ascii_lowercase())
            .unwrap_or_default();
        let length = response.content_length();
        let Some(kind) = TextKind::of(&mime) else {
            let size = length.map_or_else(|| "unknown size".to_string(), human_size);
            let shown = if mime.is_empty() { "untyped" } else { &mime };
            return Ok(ToolOutput {
                text: format!("{url} is {shown} ({size}), not text; it was not read."),
            });
        };
        let body = read_capped(response, MAX_BODY)
            .await
            .map_err(ToolError::Failed)?;
        let raw = String::from_utf8_lossy(&body.bytes);
        let text = match kind {
            TextKind::Html => html_to_markdown(&raw),
            TextKind::Plain => raw.into_owned(),
        };
        let mut out = format!("{url}\n\n");
        out.push_str(&window(&text, args.offset));
        if body.cut {
            out.push_str(&format!(
                "\n[the page is larger than {}; only its start was read]",
                human_size(MAX_BODY as u64)
            ));
        }
        Ok(ToolOutput { text: out })
    }
}

impl ToolSource for WebTools {
    fn list(&self) -> BoxFuture<'_, Result<Vec<SourceTool>, ToolError>> {
        let tools = self.catalogue();
        Box::pin(async move { Ok(tools) })
    }

    fn call(
        &self,
        name: &str,
        input: Value,
        _ctx: CallContext,
    ) -> BoxFuture<'_, Result<ToolOutput, ToolError>> {
        let name = name.to_string();
        Box::pin(async move {
            match name.as_str() {
                "search" => self.search(input).await,
                "fetch" => self.fetch(input).await,
                other => Err(ToolError::UnknownTool(other.to_string())),
            }
        })
    }

    /// A fetch is shown as the URL, which is what the user is deciding on.
    fn describe(&self, name: &str, input: &Value) -> Option<String> {
        (name == "fetch")
            .then(|| input.get("url")?.as_str().map(|u| format!("GET {u}")))
            .flatten()
    }

    /// A fetch is allowed for the chat by host. A URL that does not parse is
    /// allowed as nothing wider than itself.
    fn chat_scope(&self, name: &str, input: &Value) -> Option<ChatAllow> {
        if name != "fetch" {
            return None;
        }
        let url = input.get("url").and_then(Value::as_str).unwrap_or_default();
        Some(match Url::parse(url) {
            Ok(u) if u.host_str().is_some() => ChatAllow::Host(host_key(&u)),
            _ => ChatAllow::Call(input.to_string()),
        })
    }
}

/// `base` with `pairs` as its query, encoded.
fn with_query(base: &str, pairs: &[(&str, &str)]) -> Result<Url, ToolError> {
    let mut url = Url::parse(base)
        .map_err(|e| ToolError::Failed(format!("the search backend's URL {base}: {e}")))?;
    url.query_pairs_mut().extend_pairs(pairs);
    Ok(url)
}

fn parse<T: serde::de::DeserializeOwned>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::InvalidArguments(e.to_string()))
}

/// `text` parsed as a URL a fetch may take: http or https, with a host.
fn fetchable(text: &str) -> Result<Url, ToolError> {
    let url =
        Url::parse(text.trim()).map_err(|e| ToolError::InvalidArguments(format!("url: {e}")))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(ToolError::InvalidArguments(format!(
            "url: only http and https are fetched, not {}",
            url.scheme()
        )));
    }
    if url.host_str().is_none() {
        return Err(ToolError::InvalidArguments("url: no host".into()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ToolError::InvalidArguments(
            "url: credentials in a URL are not sent".into(),
        ));
    }
    Ok(url)
}

/// What a grant and a redirect check compare: the host and, when it is not
/// the scheme's default, the port.
fn host_key(url: &Url) -> String {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host,
    }
}

/// Refuse a host written as a non-public address, or as `localhost`. A name
/// is checked again when it resolves ([`PublicOnly`]).
fn check_literal_host(url: &Url) -> Result<(), ToolError> {
    let refused = |what: &str| {
        Err(ToolError::Refused(format!(
            "{} is {what}; web.fetch reads public sites only",
            url.host_str().unwrap_or_default()
        )))
    };
    // The URL parser has already normalised the host: an IPv4 address in any
    // spelling (`2130706433`, `0x7f.1`) is dotted, an IPv6 one bracketed.
    let host = url.host_str().unwrap_or_default();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return if is_public(ip) {
            Ok(())
        } else {
            refused("not a public address")
        };
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    if name == "localhost" || name.ends_with(".localhost") {
        return refused("this machine");
    }
    Ok(())
}

/// An address on the public internet: not loopback, private, link-local,
/// shared (CGNAT), documentation, unspecified, broadcast or multicast.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => public_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => public_v4(v4),
            None => public_v6(v6),
        },
    }
}

fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || a == 0
        // 100.64.0.0/10, shared address space (CGNAT).
        || (a == 100 && (64..128).contains(&b))
        // 192.0.0.0/24, IETF protocol assignments; 198.18.0.0/15, benchmarking.
        || (a == 192 && b == 0 && c == 0)
        || (a == 198 && (b == 18 || b == 19))
        // 240.0.0.0/4, reserved.
        || a >= 240)
}

fn public_v6(ip: Ipv6Addr) -> bool {
    let [first, second, ..] = ip.segments();
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_multicast()
        || ip.is_unique_local()
        || ip.is_unicast_link_local()
        // 2001:db8::/32, documentation.
        || (first == 0x2001 && second == 0x0db8))
}

/// A resolver that keeps only public addresses, and fails a name that has
/// none. It is the fetch client's only resolver, so redirects within a host
/// and a name re-resolved at connect time pass through it too.
struct PublicOnly;

impl reqwest::dns::Resolve for PublicOnly {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await?
                .filter(|a| is_public(a.ip()))
                .collect();
            if addrs.is_empty() {
                return Err(format!(
                    "{host} resolves to no public address; web.fetch reads public sites only"
                )
                .into());
            }
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// A fetch error as the model reads it. The resolver's refusal is in the
/// error's chain; `reqwest`'s top-level text alone says only "error sending
/// request".
fn fetch_error(url: &Url, e: &reqwest::Error) -> String {
    let mut text = format!("could not fetch {url}: {e}");
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        text.push_str(&format!(": {s}"));
        source = s.source();
    }
    if e.is_timeout() {
        text.push_str(&format!(" (after {}s)", REQUEST_TIMEOUT.as_secs()));
    }
    text
}

/// The text kinds a fetch converts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextKind {
    Html,
    Plain,
}

impl TextKind {
    fn of(mime: &str) -> Option<Self> {
        match mime {
            // A server that names no type is most often serving a page.
            "text/html" | "application/xhtml+xml" | "" => Some(Self::Html),
            m if m.starts_with("text/") => Some(Self::Plain),
            "application/json"
            | "application/xml"
            | "application/rss+xml"
            | "application/atom+xml" => Some(Self::Plain),
            m if m.ends_with("+json") || m.ends_with("+xml") => Some(Self::Plain),
            _ => None,
        }
    }
}

/// Markdown for `html`: scripts, styles and other non-content left out. A
/// page the converter cannot handle comes back as its bare text.
fn html_to_markdown(html: &str) -> String {
    let converter = htmd::HtmlToMarkdown::builder()
        .skip_tags(vec![
            "script", "style", "noscript", "template", "svg", "iframe", "head", "nav", "footer",
            "form", "button",
        ])
        .build();
    let text = converter.convert(html).unwrap_or_else(|_| strip_tags(html));
    collapse_blank_lines(&text)
}

/// Text with every `<...>` removed: the converter's fallback, and search
/// snippets, which carry `<strong>` around the matched words.
fn strip_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    decode_entities(&out)
}

fn decode_entities(text: &str) -> String {
    text.replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
}

/// At most one blank line in a row, and no trailing blanks on a line.
fn collapse_blank_lines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut blank = 0;
    for line in text.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            blank += 1;
            if blank > 1 {
                continue;
            }
        } else {
            blank = 0;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim().to_string()
}

/// `text` from character `offset`, at most [`MAX_TEXT`] characters, with a
/// line saying how to read on when there is more.
fn window(text: &str, offset: usize) -> String {
    let total = text.chars().count();
    if offset >= total && total > 0 {
        return format!("[offset {offset} is past the end: the page has {total} characters]");
    }
    let shown: String = text.chars().skip(offset).take(MAX_TEXT).collect();
    let end = offset + shown.chars().count();
    if end < total {
        format!("{shown}\n\n[characters {offset}–{end} of {total}; pass offset={end} to read on]")
    } else {
        shown
    }
}

/// A body read up to a cap.
struct Body {
    bytes: Vec<u8>,
    /// The body went on past the cap.
    cut: bool,
}

async fn read_capped(mut response: reqwest::Response, cap: usize) -> Result<Body, String> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("reading the response failed: {e}"))?
    {
        let room = cap - bytes.len();
        if chunk.len() > room {
            bytes.extend_from_slice(&chunk[..room]);
            return Ok(Body { bytes, cut: true });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Body { bytes, cut: false })
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// The Brave Search API's response: `web.results[].{title,url,description}`.
pub fn decode_brave(body: &[u8]) -> Result<Vec<Hit>, String> {
    #[derive(Deserialize)]
    struct Response {
        web: Option<Web>,
    }
    #[derive(Deserialize)]
    struct Web {
        #[serde(default)]
        results: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(default)]
        title: String,
        url: String,
        #[serde(default)]
        description: String,
    }
    let r: Response = serde_json::from_slice(body)
        .map_err(|e| format!("the search backend's answer is not what brave sends: {e}"))?;
    Ok(r.web
        .map(|w| w.results)
        .unwrap_or_default()
        .into_iter()
        .map(|i| Hit {
            title: strip_tags(&i.title),
            url: i.url,
            snippet: strip_tags(&i.description),
        })
        .collect())
}

/// SearXNG's `format=json`: `results[].{title,url,content}`.
pub fn decode_searxng(body: &[u8]) -> Result<Vec<Hit>, String> {
    #[derive(Deserialize)]
    struct Response {
        #[serde(default)]
        results: Vec<Item>,
    }
    #[derive(Deserialize)]
    struct Item {
        #[serde(default)]
        title: String,
        url: String,
        #[serde(default)]
        content: String,
    }
    let r: Response = serde_json::from_slice(body).map_err(|e| {
        format!(
            "the search backend's answer is not SearXNG JSON ({e}); is `json` enabled in its \
             settings.yml `search.formats`?"
        )
    })?;
    Ok(r.results
        .into_iter()
        .map(|i| Hit {
            title: strip_tags(&i.title),
            url: i.url,
            snippet: strip_tags(&i.content),
        })
        .collect())
}

fn format_hits(query: &str, hits: &[Hit], count: usize) -> String {
    if hits.is_empty() {
        return format!("No results for {query:?}.");
    }
    let mut out = String::new();
    for (n, hit) in hits.iter().take(count).enumerate() {
        out.push_str(&format!(
            "{}. {}\n   {}\n",
            n + 1,
            hit.title.trim(),
            hit.url
        ));
        let snippet = hit.snippet.trim();
        if !snippet.is_empty() {
            out.push_str(&format!("   {snippet}\n"));
        }
    }
    out.trim_end().to_string()
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default();
    line.chars().take(200).collect()
}

fn human_size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= 1024 {
        format!("{} KiB", bytes / 1024)
    } else {
        format!("{bytes} bytes")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn config(web: WebConfig) -> WebConfigFn {
        Arc::new(move || web.clone())
    }

    /// Over loopback, which the real client refuses: the address filter has
    /// tests of its own below.
    fn tools_over_loopback(web: WebConfig) -> WebTools {
        WebTools::build(config(web), false).unwrap()
    }

    /// A server answering each connection with the next canned response, and
    /// recording each request's head.
    async fn serve(responses: Vec<String>) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            for response in responses {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 8192];
                let n = socket.read(&mut buf).await.unwrap_or(0);
                log.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf[..n]).into_owned());
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            }
        });
        (format!("http://{addr}"), seen)
    }

    fn http(status: &str, headers: &[(&str, &str)], body: &str) -> String {
        let mut r = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\n", body.len());
        for (k, v) in headers {
            r.push_str(&format!("{k}: {v}\r\n"));
        }
        r.push_str("Connection: close\r\n\r\n");
        r.push_str(body);
        r
    }

    async fn fetch(tools: &WebTools, input: Value) -> Result<String, String> {
        tools
            .call("fetch", input, CallContext::default())
            .await
            .map(|o| o.text)
            .map_err(|e| e.to_string())
    }

    #[test]
    fn brave_results_decode_with_their_markup_removed() {
        let body = br#"{"web":{"results":[
            {"title":"The <strong>Rust</strong> Book","url":"https://doc.rust-lang.org/book/",
             "description":"Learn <strong>Rust</strong> &amp; more"},
            {"url":"https://example.org/"}]}}"#;
        let hits = decode_brave(body).unwrap();
        assert_eq!(hits[0].title, "The Rust Book");
        assert_eq!(hits[0].snippet, "Learn Rust & more");
        assert_eq!(hits[1].url, "https://example.org/");
        assert!(decode_brave(br#"{"query":{}}"#).unwrap().is_empty());
        assert!(decode_brave(b"<html>").is_err());
    }

    #[test]
    fn searxng_results_decode_and_html_answers_say_json_is_off() {
        let body = br#"{"results":[{"title":"A","url":"https://a.example/","content":"alpha"}]}"#;
        assert_eq!(
            decode_searxng(body).unwrap(),
            vec![Hit {
                title: "A".into(),
                url: "https://a.example/".into(),
                snippet: "alpha".into()
            }]
        );
        let err = decode_searxng(b"<!doctype html>").unwrap_err();
        assert!(err.contains("search.formats"), "{err}");
    }

    #[tokio::test]
    async fn searxng_is_asked_over_its_own_client_and_results_are_listed() {
        let body = r#"{"results":[{"title":"One","url":"https://one.example/","content":"first"},
                      {"title":"Two","url":"https://two.example/","content":""}]}"#;
        let (base, seen) = serve(vec![http(
            "200 OK",
            &[("Content-Type", "application/json")],
            body,
        )])
        .await;
        // The fetch filter is on: a SearXNG on localhost is the user's choice.
        let tools = WebTools::new(config(WebConfig {
            search: Some(SearchBackend::Searxng { base_url: base }),
        }))
        .unwrap();
        let out = tools
            .call(
                "search",
                json!({"query": "rust fold", "count": 1}),
                CallContext::default(),
            )
            .await
            .unwrap()
            .text;
        assert_eq!(out, "1. One\n   https://one.example/\n   first");
        let request = seen.lock().unwrap()[0].clone();
        assert!(
            request.starts_with("GET /search?q=rust+fold&format=json"),
            "{request}"
        );
    }

    #[tokio::test]
    async fn search_is_listed_only_with_a_backend() {
        let names = |tools: &WebTools| -> Vec<String> {
            tools
                .catalogue()
                .into_iter()
                .map(|t| t.descriptor.name)
                .collect()
        };
        assert_eq!(names(&tools_over_loopback(WebConfig::default())), ["fetch"]);
        let with = tools_over_loopback(WebConfig {
            search: Some(SearchBackend::Searxng {
                base_url: "http://x".into(),
            }),
        });
        assert_eq!(names(&with), ["search", "fetch"]);
        // A fetch is never a read: the `read` group runs unasked.
        assert!(
            with.catalogue()
                .iter()
                .all(|t| t.descriptor.name != "fetch" || t.kind == ToolKind::Other)
        );
    }

    #[tokio::test]
    async fn a_missing_brave_key_is_named_and_nothing_is_sent() {
        let var = "LATTICE_AT2_BRAVE_KEY_SURELY_UNSET";
        let tools = tools_over_loopback(WebConfig {
            search: Some(SearchBackend::Brave {
                key: crate::llm::config::KeySource::Env(var.into()),
            }),
        });
        let err = tools
            .call("search", json!({"query": "x"}), CallContext::default())
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(var) && err.contains("not set"), "{err}");
    }

    #[tokio::test]
    async fn a_page_is_markdown_with_its_links_and_without_its_scripts() {
        let page = "<html><head><title>T</title><script>evil()</script></head><body>\
                    <h1>Folding</h1><p>See <a href=\"https://docs.example/fold\">the docs</a>.</p>\
                    <style>p{}</style></body></html>";
        let (base, _) = serve(vec![http(
            "200 OK",
            &[("Content-Type", "text/html; charset=utf-8")],
            page,
        )])
        .await;
        let out = fetch(
            &tools_over_loopback(WebConfig::default()),
            json!({"url": format!("{base}/page")}),
        )
        .await
        .unwrap();
        assert!(out.contains("# Folding"), "{out}");
        assert!(
            out.contains("[the docs](https://docs.example/fold)"),
            "{out}"
        );
        assert!(!out.contains("evil") && !out.contains("p{}"), "{out}");
    }

    #[tokio::test]
    async fn a_redirect_within_the_host_is_followed_and_to_another_is_reported() {
        let (base, _) = serve(vec![
            http("301 Moved", &[("Location", "/final")], ""),
            http("200 OK", &[("Content-Type", "text/plain")], "arrived"),
        ])
        .await;
        let tools = tools_over_loopback(WebConfig::default());
        let out = fetch(&tools, json!({"url": format!("{base}/start")}))
            .await
            .unwrap();
        assert!(out.ends_with("arrived"), "{out}");

        let (base, seen) = serve(vec![http(
            "302 Found",
            &[("Location", "https://elsewhere.example/x")],
            "",
        )])
        .await;
        let out = fetch(&tools, json!({"url": format!("{base}/start")}))
            .await
            .unwrap();
        assert!(
            out.contains("redirects to https://elsewhere.example/x, on another site"),
            "{out}"
        );
        assert_eq!(
            seen.lock().unwrap().len(),
            1,
            "the other host was not asked"
        );
    }

    #[tokio::test]
    async fn a_binary_answer_is_named_and_not_read() {
        let (base, _) = serve(vec![http(
            "200 OK",
            &[("Content-Type", "application/pdf")],
            "%PDF-1.7 ...",
        )])
        .await;
        let out = fetch(
            &tools_over_loopback(WebConfig::default()),
            json!({"url": format!("{base}/x.pdf")}),
        )
        .await
        .unwrap();
        assert!(
            out.contains("is application/pdf (12 bytes), not text"),
            "{out}"
        );
        assert!(!out.contains("%PDF"));
    }

    #[tokio::test]
    async fn a_large_body_stops_at_the_cap_and_a_long_page_reads_on() {
        let body = "a".repeat(MAX_BODY + 100);
        let (base, _) = serve(vec![http(
            "200 OK",
            &[("Content-Type", "text/plain")],
            &body,
        )])
        .await;
        let out = fetch(
            &tools_over_loopback(WebConfig::default()),
            json!({"url": format!("{base}/big"), "offset": 10}),
        )
        .await
        .unwrap();
        let end = 10 + MAX_TEXT;
        assert!(
            out.contains(&format!(
                "characters 10–{end} of {MAX_BODY}; pass offset={end}"
            )),
            "{}",
            &out[out.len() - 300..]
        );
        assert!(out.ends_with("only its start was read]"));
    }

    #[tokio::test]
    async fn an_error_status_is_the_calls_error() {
        let (base, _) = serve(vec![http("404 Not Found", &[], "")]).await;
        let err = fetch(
            &tools_over_loopback(WebConfig::default()),
            json!({"url": format!("{base}/gone")}),
        )
        .await
        .unwrap_err();
        assert!(err.contains("404"), "{err}");
    }

    #[tokio::test]
    async fn the_real_client_refuses_this_machine_and_private_networks() {
        let tools = WebTools::new(config(WebConfig::default())).unwrap();
        for url in [
            "http://127.0.0.1:9/",
            "http://localhost/",
            "http://app.localhost/",
            "http://10.0.0.1/",
            "http://192.168.1.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:127.0.0.1]/",
        ] {
            let err = fetch(&tools, json!({ "url": url })).await.unwrap_err();
            assert!(err.contains("public sites only"), "{url}: {err}");
        }
    }

    #[tokio::test]
    async fn the_resolver_drops_private_answers_and_fails_a_name_with_none() {
        use reqwest::dns::Resolve;
        // `localhost` resolves without the network, to loopback only. The
        // literal check would catch it first; this is the second line, for a
        // name whose answer is private.
        let name: reqwest::dns::Name = "localhost".parse().unwrap();
        let err = match PublicOnly.resolve(name).await {
            Ok(_) => panic!("localhost resolved to a public address"),
            Err(e) => e.to_string(),
        };
        assert!(err.contains("no public address"), "{err}");
    }

    #[test]
    fn public_addresses_are_told_from_the_rest() {
        for ip in ["93.184.216.34", "1.1.1.1", "2606:4700:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.0.1",
            "169.254.1.1",
            "100.64.0.1",
            "0.0.0.0",
            "255.255.255.255",
            "224.0.0.1",
            "198.18.0.1",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "2001:db8::1",
            "::ffff:10.0.0.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn a_fetch_is_granted_by_host_and_port() {
        let tools = tools_over_loopback(WebConfig::default());
        let scope = |url: &str| tools.chat_scope("fetch", &json!({ "url": url }));
        assert_eq!(
            scope("https://Docs.RS/serde/latest"),
            Some(ChatAllow::Host("docs.rs".into()))
        );
        assert_eq!(
            scope("https://docs.rs:8443/x"),
            Some(ChatAllow::Host("docs.rs:8443".into()))
        );
        assert!(matches!(scope("not a url"), Some(ChatAllow::Call(_))));
        assert_eq!(tools.chat_scope("search", &json!({})), None);
        assert_eq!(
            tools.describe("fetch", &json!({"url": "https://a.example/"})),
            Some("GET https://a.example/".into())
        );
    }

    #[test]
    fn only_web_urls_without_credentials_are_fetched() {
        assert!(fetchable("https://a.example/x").is_ok());
        for bad in [
            "file:///etc/passwd",
            "ftp://a.example/",
            "https://u:p@a.example/",
            "x",
        ] {
            assert!(fetchable(bad).is_err(), "{bad}");
        }
    }
}
