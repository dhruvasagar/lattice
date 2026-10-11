//! LH.0.1 — the `host-services.http-download` seam.
//!
//! Design: `docs/dev/architecture/lighthouse.md` §3.0–§3.1. Slice plan:
//! `docs/dev/operations/slice-plans/lighthouse.md` LH.0.1.
//!
//! ## Why a request and not a call
//!
//! `host-services` is wired on the grammar seam's synchronous linker, so a
//! guest call into it can be running on the editor's dispatch thread; a guest
//! call also has a wall-clock budget that host time counts against. A function
//! that returned when the transfer finished would freeze the editor from one
//! seam and trap from the others. So [`prepare`] validates and returns a
//! [`PendingJob`]; a thread does the transfer, and the outcome is an
//! `Event::JobFinished` addressed to the plugin that asked.
//!
//! ## What is checked, and when
//!
//! Everything a plugin author can get wrong in a manifest is refused
//! **synchronously**, by [`prepare`]: the first URL's host against the
//! `net:http` grant, the destination against `fs:write`, the digest's syntax.
//! A refusal that arrived as an event some seconds later would read as a flaky
//! network rather than as a missing grant.
//!
//! What can only be known by asking the network arrives as the event: an HTTP
//! status, a redirect to a host that is not granted, a body that does not hash
//! to what was pinned.
//!
//! ## Redirects are followed here, one hop at a time
//!
//! The client is configured to follow none. Each `Location` is resolved and
//! put through the same grant check as the URL the guest named, because a
//! client that followed redirects on its own would turn a grant for one host
//! into a grant for whichever host that server chose to name.
//!
//! ## The part file is the integrity mechanism
//!
//! The body streams to `<dest>.part` while being hashed, and only a matching
//! digest renames it into place. Every other way out of [`transfer`] removes
//! the part file. So a plugin never has to remember to verify, and "the file
//! exists" is a sound test for "the file was verified".
//!
//! ## Lifetime and cancellation
//!
//! A download is a host job — see [`crate::job`] for the id, the addressed
//! events, the guard that cancels on drop and cancel-by-id.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lattice_runtime::EventBus;
use sha2::{Digest as _, Sha256};
use ureq::http::Uri;
use ureq::http::header::{CONTENT_LENGTH, LOCATION};

use crate::capability::CapabilityGrant;
use crate::job::{Job, PendingJob};

/// The bounds a transfer runs under. Host policy: a guest cannot widen them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    /// Largest body accepted. Checked against `Content-Length` up front and
    /// against the running count, since a server may send no length or lie.
    max_bytes: u64,
    /// Redirect hops followed before giving up.
    max_redirects: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            // 1 GiB. The largest language-server release archives are a few
            // hundred MB; anything past this is a mistake or an attack, and
            // either way not something to fill a disk with.
            max_bytes: 1 << 30,
            max_redirects: 5,
        }
    }
}

/// How long to wait for a TCP + TLS connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// How long to wait for response headers once the request is sent.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
/// Ceiling on receiving one body. A total rather than a stall timeout — the
/// client offers only the former — so it is set where no honest download of
/// [`Limits::max_bytes`] reaches it, and exists to bound a connection that has
/// gone silent, which a cancel cannot interrupt mid-read.
const BODY_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// One validated download: what the transfer thread needs and nothing else.
#[derive(Debug)]
struct Request {
    url: String,
    /// Lowercase hex, already validated.
    sha256: String,
    dest: PathBuf,
    /// The grant's `net:http` entries, lowercased — every redirect hop is
    /// checked against these on the transfer thread.
    hosts: Vec<String>,
    limits: Limits,
}

/// Validate a download request against `grant` and make it a job.
///
/// Refuses, in words naming which: a digest that is not 64 hex digits, a URL
/// that is malformed / not `https` / on an ungranted host, and a destination
/// outside the plugin's `fs:write` prefixes.
pub(crate) fn prepare(
    grant: &CapabilityGrant,
    bus: Arc<EventBus>,
    plugin: u32,
    url: &str,
    sha256: &str,
    dest: &str,
) -> Result<PendingJob, String> {
    prepare_with(grant, bus, plugin, url, sha256, dest, Limits::default())
}

fn prepare_with(
    grant: &CapabilityGrant,
    bus: Arc<EventBus>,
    plugin: u32,
    url: &str,
    sha256: &str,
    dest: &str,
    limits: Limits,
) -> Result<PendingJob, String> {
    if sha256.len() != 64 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(format!(
            "http download failed: '{sha256}' is not a SHA-256 digest (64 hex digits)"
        ));
    }
    let hosts: Vec<String> = grant
        .net_http
        .iter()
        .map(|h| h.to_ascii_lowercase())
        .collect();
    if let Err(e) = check_url(&hosts, url) {
        // info!: user-actionable (a plugin was denied network access), the
        // level the fs seams' denials use.
        tracing::info!(url, error = %e, "host-services http-download refused");
        return Err(e);
    }
    let dest_path = PathBuf::from(dest);
    if !crate::host_services::grant_permits_write(grant, &dest_path) {
        tracing::info!(
            dest,
            "host-services http-download denied: outside the plugin's fs:write grant"
        );
        return Err(format!(
            "http download denied: '{dest}' is outside the plugin's writable paths"
        ));
    }
    if dest_path.is_dir() {
        return Err(format!("http download failed: '{dest}' is a directory"));
    }
    let request = Request {
        url: url.to_string(),
        sha256: sha256.to_ascii_lowercase(),
        dest: dest_path,
        hosts,
        limits,
    };
    Ok(PendingJob::new(plugin, bus, "download", move |job| {
        transfer(&request, job)
    }))
}

/// Is `url` one this plugin may fetch?
///
/// `https`, or `http` to a loopback address; and the host — with its effective
/// port, when the grant entry names one — in `hosts`.
fn check_url(hosts: &[String], url: &str) -> Result<(), String> {
    let uri: Uri = url
        .parse()
        .map_err(|e| format!("http download failed: '{url}' is not a URL: {e}"))?;
    let Some(host) = uri.host() else {
        return Err(format!("http download failed: '{url}' names no host"));
    };
    let host = host.to_ascii_lowercase();
    let port = match uri.scheme_str() {
        Some("https") => uri.port_u16().unwrap_or(443),
        Some("http") => {
            if !is_loopback(&host) {
                return Err(format!(
                    "http download denied: '{url}' is not https (plain http is accepted \
                     only for a loopback address)"
                ));
            }
            uri.port_u16().unwrap_or(80)
        }
        _ => {
            return Err(format!(
                "http download failed: '{url}' is not an http(s) URL"
            ));
        }
    };
    let with_port = format!("{host}:{port}");
    if hosts.iter().any(|h| *h == host || *h == with_port) {
        Ok(())
    } else {
        Err(format!(
            "http download denied: host '{host}' is not in the plugin's net:http grant"
        ))
    }
}

fn is_loopback(host: &str) -> bool {
    // `Uri::host` keeps an IPv6 literal's brackets.
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    bare == "localhost"
        || bare
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Resolve a `Location` header against the URL that sent it.
///
/// Absolute URLs and absolute paths only. A path-relative `Location` is legal
/// HTTP and vanishingly rare from a release host; refusing it by name is more
/// honest than a hand-rolled resolver that is subtly wrong about `..`.
fn resolve_redirect(from: &str, location: &str) -> Result<String, String> {
    if let Ok(uri) = location.parse::<Uri>()
        && uri.scheme().is_some()
    {
        return Ok(location.to_string());
    }
    if location.starts_with('/') {
        let base: Uri = from
            .parse()
            .map_err(|e| format!("http download failed: '{from}' is not a URL: {e}"))?;
        if let (Some(scheme), Some(authority)) = (base.scheme_str(), base.authority()) {
            return Ok(format!("{scheme}://{authority}{location}"));
        }
    }
    Err(format!(
        "http download failed: '{from}' redirected to '{location}', which is not an \
         absolute URL or path"
    ))
}

/// `<dest>.part` — a sibling, so the final rename never crosses a filesystem.
fn part_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

/// Run one download to its end. `Ok` only with the verified file at `dest`;
/// on every `Err` the part file is gone.
fn transfer(req: &Request, job: &mut Job) -> Result<(), String> {
    let part = part_path(&req.dest);
    let result = fetch_into(req, job, &part);
    if result.is_err() {
        // Absent is fine: most failures happen before the file is created.
        let _ = std::fs::remove_file(&part);
    }
    result
}

fn fetch_into(req: &Request, job: &mut Job, part: &Path) -> Result<(), String> {
    let config = ureq::Agent::config_builder()
        // Redirects are followed below, one grant-checked hop at a time.
        .max_redirects(0)
        .max_redirects_will_error(false)
        // A 404 is an outcome to report, not a transport error.
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .timeout_recv_body(Some(BODY_TIMEOUT))
        .build();
    let agent = ureq::Agent::new_with_config(config);

    let mut url = req.url.clone();
    let mut hops = 0;
    let mut response = loop {
        job.check_cancelled()?;
        // The first URL was checked in `prepare`; re-checking it here costs a
        // parse and keeps this loop the one place a request can leave from.
        check_url(&req.hosts, &url)?;
        let response = agent
            .get(&url)
            .call()
            .map_err(|e| format!("http download failed: GET {url}: {e}"))?;
        let status = response.status();
        if status.is_redirection() {
            let Some(location) = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
            else {
                return Err(format!(
                    "http download failed: GET {url}: HTTP {status} with no usable Location"
                ));
            };
            hops += 1;
            if hops > req.limits.max_redirects {
                return Err(format!(
                    "http download failed: more than {} redirects from '{}'",
                    req.limits.max_redirects, req.url
                ));
            }
            url = resolve_redirect(&url, location)?;
            continue;
        }
        if !status.is_success() {
            return Err(format!("http download failed: GET {url}: HTTP {status}"));
        }
        break response;
    };

    let total = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let too_big = |n: u64| {
        format!(
            "http download failed: '{}' is larger than the {} byte limit ({n} bytes)",
            req.url, req.limits.max_bytes
        )
    };
    if let Some(n) = total
        && n > req.limits.max_bytes
    {
        return Err(too_big(n));
    }

    if let Some(parent) = req.dest.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).map_err(|e| {
            format!(
                "http download failed: cannot create '{}': {e}",
                parent.display()
            )
        })?;
    }
    let mut file = std::fs::File::create(part).map_err(|e| {
        format!(
            "http download failed: cannot create '{}': {e}",
            part.display()
        )
    })?;

    let mut body = response.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut received: u64 = 0;
    loop {
        job.check_cancelled()?;
        let n = body
            .read(&mut buf)
            .map_err(|e| format!("http download failed: reading {url}: {e}"))?;
        if n == 0 {
            break;
        }
        received += n as u64;
        if received > req.limits.max_bytes {
            return Err(too_big(received));
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|e| format!("http download failed: writing '{}': {e}", part.display()))?;
        job.progress(received, total);
    }
    if let Some(n) = total
        && received != n
    {
        return Err(format!(
            "http download failed: '{url}' ended after {received} of {n} bytes"
        ));
    }

    let digest: String = hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if digest != req.sha256 {
        return Err(format!(
            "http download failed: sha256 mismatch for '{}': expected {}, got {digest}",
            req.url, req.sha256
        ));
    }

    // Flushed to the device before the rename, so a crash cannot leave a
    // correctly-named file whose contents never reached the disk.
    file.sync_all()
        .map_err(|e| format!("http download failed: syncing '{}': {e}", part.display()))?;
    drop(file);
    std::fs::rename(part, &req.dest).map_err(|e| {
        format!(
            "http download failed: cannot move the download to '{}': {e}",
            req.dest.display()
        )
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::panic)]

    use super::*;
    use crate::capability::FsGrant;
    use crate::job::test_support::{Outcome, job_bus, outcome_of};
    use crate::job::{CANCELLED, JobGuard};
    use lattice_protocol::Event as NativeEvent;
    use std::io::{BufRead as _, BufReader};
    use std::net::{TcpListener, TcpStream};
    use tokio::sync::mpsc::UnboundedReceiver;

    const BODY: &[u8] = b"a language server, or near enough\n";

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A loopback HTTP server with a handful of canned routes — enough to be a
    /// release host, a redirecting one, a slow one and a broken one.
    struct Server {
        port: u16,
    }

    impl Server {
        fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            std::thread::spawn(move || {
                for stream in listener.incoming().flatten() {
                    std::thread::spawn(move || serve(stream, port));
                }
            });
            Self { port }
        }

        /// By IP — the spelling the tests grant.
        fn url(&self, path: &str) -> String {
            format!("http://127.0.0.1:{}{path}", self.port)
        }

        fn host(&self) -> String {
            "127.0.0.1".to_string()
        }
    }

    fn serve(mut stream: TcpStream, port: u16) {
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            return;
        }
        let path = line.split_whitespace().nth(1).unwrap_or("/").to_string();
        let ok = |body: &[u8]| {
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            out.extend_from_slice(body);
            out
        };
        let redirect = |to: &str| {
            format!(
                "HTTP/1.1 302 Found\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .into_bytes()
        };
        match path.as_str() {
            "/file" => {
                let _ = stream.write_all(&ok(BODY));
            }
            // An absolute-path redirect on the same, granted, host.
            "/moved" => {
                let _ = stream.write_all(&redirect("/file"));
            }
            // The same server under a name the tests do NOT grant. Loopback,
            // so it passes the scheme rule and reaches the grant check — which
            // is the check under test.
            "/elsewhere" => {
                let _ = stream.write_all(&redirect(&format!("http://localhost:{port}/file")));
            }
            "/loop" => {
                let _ = stream.write_all(&redirect("/loop"));
            }
            // Trickles for ~2 s: long enough to cancel, and to see progress.
            "/slow" => {
                let chunks = 40;
                let chunk = [b'x'; 1024];
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    chunks * chunk.len()
                );
                if stream.write_all(head.as_bytes()).is_err() {
                    return;
                }
                for _ in 0..chunks {
                    if stream.write_all(&chunk).is_err() || stream.flush().is_err() {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            // Promises more than it sends.
            "/truncated" => {
                let _ = stream.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nshort",
                );
            }
            _ => {
                let _ = stream.write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                );
            }
        }
    }

    fn grant(host: &str, writable: &Path) -> CapabilityGrant {
        CapabilityGrant {
            fs: vec![FsGrant {
                prefix: writable.to_path_buf(),
                write: true,
            }],
            net_http: vec![host.to_string()],
            ..Default::default()
        }
    }

    /// One download from request to outcome, with the pieces a test inspects.
    struct Run {
        _dir: tempfile::TempDir,
        dest: PathBuf,
        outcome: Outcome,
    }

    fn run(path: &str, sha256: &str, limits: Limits) -> Run {
        let server = Server::start();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("bin").join("server");
        let bus = Arc::new(EventBus::new());
        let mut rx = job_bus(&bus);
        let pending = prepare_with(
            &grant(&server.host(), dir.path()),
            bus,
            7,
            &server.url(path),
            sha256,
            dest.to_str().unwrap(),
            limits,
        )
        .expect("a granted host and destination are accepted");
        let id = pending.id();
        let _guard = pending.start();
        let outcome = outcome_of(&mut rx, id);
        Run {
            _dir: dir,
            dest,
            outcome,
        }
    }

    fn assert_nothing_left(dest: &Path) {
        assert!(!dest.exists(), "no file at the destination");
        assert!(!part_path(dest).exists(), "and no part file beside it");
    }

    #[test]
    fn a_verified_download_lands_at_its_destination() {
        let r = run("/file", &sha(BODY), Limits::default());
        assert_eq!(r.outcome.result, Ok(()));
        assert_eq!(
            r.outcome.plugin, 7,
            "addressed to the plugin that started it"
        );
        assert_eq!(std::fs::read(&r.dest).unwrap(), BODY);
        assert!(
            !part_path(&r.dest).exists(),
            "the part file was renamed away"
        );
    }

    /// An uppercase digest is the same digest.
    #[test]
    fn the_digest_is_compared_case_insensitively() {
        let r = run("/file", &sha(BODY).to_ascii_uppercase(), Limits::default());
        assert_eq!(r.outcome.result, Ok(()));
    }

    /// **The supply-chain case.** The body arrived intact and is simply not
    /// what was pinned.
    #[test]
    fn a_digest_mismatch_leaves_nothing_on_disk() {
        let wrong = sha(b"something else entirely");
        let r = run("/file", &wrong, Limits::default());
        let err = r.outcome.result.expect_err("a mismatch is a failure");
        assert!(err.contains("sha256 mismatch"), "{err}");
        assert!(err.contains(&wrong), "names what was expected: {err}");
        assert!(err.contains(&sha(BODY)), "and what arrived: {err}");
        assert_nothing_left(&r.dest);
    }

    /// A failed update must not cost the user the version they had.
    #[test]
    fn a_failed_download_leaves_an_existing_destination_alone() {
        let server = Server::start();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("server");
        std::fs::write(&dest, b"the old version").unwrap();
        let bus = Arc::new(EventBus::new());
        let mut rx = job_bus(&bus);
        let pending = prepare(
            &grant(&server.host(), dir.path()),
            bus,
            1,
            &server.url("/file"),
            &sha(b"not this"),
            dest.to_str().unwrap(),
        )
        .unwrap();
        let id = pending.id();
        let _guard = pending.start();
        assert!(outcome_of(&mut rx, id).result.is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"the old version");
        assert!(!part_path(&dest).exists());
    }

    #[test]
    fn a_redirect_within_the_grant_is_followed() {
        let r = run("/moved", &sha(BODY), Limits::default());
        assert_eq!(r.outcome.result, Ok(()));
        assert_eq!(std::fs::read(&r.dest).unwrap(), BODY);
    }

    /// **One granted host must not be a door to any host.** The redirect
    /// target serves the right bytes with the right hash — the only thing
    /// wrong with it is that nobody granted it.
    #[test]
    fn a_redirect_to_an_ungranted_host_is_refused_by_name() {
        let r = run("/elsewhere", &sha(BODY), Limits::default());
        let err = r.outcome.result.expect_err("the hop is not granted");
        assert!(err.contains("denied"), "{err}");
        assert!(err.contains("'localhost'"), "names the host: {err}");
        assert_nothing_left(&r.dest);
    }

    #[test]
    fn a_redirect_loop_ends() {
        let r = run("/loop", &sha(BODY), Limits::default());
        let err = r.outcome.result.expect_err("a loop is a failure");
        assert!(err.contains("redirects"), "{err}");
        assert_nothing_left(&r.dest);
    }

    #[test]
    fn an_http_error_status_is_reported() {
        let r = run("/missing", &sha(BODY), Limits::default());
        let err = r.outcome.result.expect_err("a 404 is a failure");
        assert!(err.contains("404"), "{err}");
        assert_nothing_left(&r.dest);
    }

    #[test]
    fn a_body_over_the_limit_is_refused() {
        let limits = Limits {
            max_bytes: 8,
            ..Limits::default()
        };
        let r = run("/file", &sha(BODY), limits);
        let err = r.outcome.result.expect_err("over the cap");
        assert!(err.contains("limit"), "{err}");
        assert_nothing_left(&r.dest);
    }

    #[test]
    fn a_truncated_body_is_a_failure_not_a_short_file() {
        let r = run("/truncated", &sha(b"short"), Limits::default());
        assert!(r.outcome.result.is_err(), "{:?}", r.outcome.result);
        assert_nothing_left(&r.dest);
    }

    /// Progress is published while the body arrives, carries the total, and
    /// only ever counts up.
    #[test]
    fn progress_is_reported_while_the_body_arrives() {
        let r = run("/slow", &sha(&[b'x'; 40 * 1024]), Limits::default());
        assert_eq!(r.outcome.result, Ok(()));
        let p = &r.outcome.progress;
        assert!(!p.is_empty(), "a ~2 s body reported progress");
        assert!(
            p.len() < 40,
            "coalesced: {} deliveries for 40 chunks",
            p.len()
        );
        assert!(p.iter().all(|(_, total)| *total == Some(40 * 1024)));
        assert!(p.windows(2).all(|w| w[0].0 <= w[1].0), "monotonic: {p:?}");
    }

    /// A slow download in flight, for the cancellation tests.
    struct Slow {
        _dir: tempfile::TempDir,
        dest: PathBuf,
        id: u64,
        guard: Option<JobGuard>,
        rx: UnboundedReceiver<NativeEvent>,
    }

    fn start_slow(plugin: u32) -> Slow {
        let server = Server::start();
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("server");
        let bus = Arc::new(EventBus::new());
        let rx = job_bus(&bus);
        let pending = prepare(
            &grant(&server.host(), dir.path()),
            bus,
            plugin,
            &server.url("/slow"),
            &sha(&[b'x'; 40 * 1024]),
            dest.to_str().unwrap(),
        )
        .unwrap();
        let id = pending.id();
        let guard = Some(pending.start());
        Slow {
            _dir: dir,
            dest,
            id,
            guard,
            rx,
        }
    }

    #[test]
    fn a_cancelled_download_reports_and_leaves_nothing() {
        let mut s = start_slow(3);
        std::thread::sleep(Duration::from_millis(200));
        crate::job::cancel(3, s.id);
        let err = outcome_of(&mut s.rx, s.id)
            .result
            .expect_err("a cancel is reported as a failure");
        assert_eq!(err, CANCELLED);
        assert_nothing_left(&s.dest);
    }

    /// Unload and quarantine both reduce to this.
    #[test]
    fn dropping_the_guard_cancels_the_transfer() {
        let mut s = start_slow(3);
        std::thread::sleep(Duration::from_millis(200));
        s.guard = None;
        let err = outcome_of(&mut s.rx, s.id)
            .result
            .expect_err("the owner went away");
        assert_eq!(err, CANCELLED);
        assert_nothing_left(&s.dest);
    }

    // ---- the synchronous refusals -------------------------------------

    fn refused(grant: &CapabilityGrant, url: &str, sha256: &str, dest: &Path) -> String {
        prepare(
            grant,
            Arc::new(EventBus::new()),
            1,
            url,
            sha256,
            dest.to_str().unwrap(),
        )
        .expect_err("refused")
    }

    #[test]
    fn an_ungranted_host_is_refused_at_the_call() {
        let dir = tempfile::tempdir().unwrap();
        let err = refused(
            &grant("github.com", dir.path()),
            "https://example.com/x",
            &sha(BODY),
            &dir.path().join("x"),
        );
        assert!(err.contains("denied"), "{err}");
        assert!(err.contains("'example.com'"), "{err}");
        assert!(err.contains("net:http"), "names the grant to fix: {err}");
    }

    #[test]
    fn a_plugin_with_no_net_grant_reaches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = grant("unused", dir.path());
        g.net_http.clear();
        let err = refused(
            &g,
            "https://github.com/x",
            &sha(BODY),
            &dir.path().join("x"),
        );
        assert!(err.contains("denied"), "{err}");
    }

    /// A subdomain is a different host. `github.com` does not grant
    /// `github.com.attacker.net`, nor `objects.github.com`.
    #[test]
    fn the_host_match_is_exact() {
        let hosts = vec!["github.com".to_string()];
        assert!(check_url(&hosts, "https://github.com/a").is_ok());
        assert!(check_url(&hosts, "https://GitHub.com/a").is_ok());
        assert!(check_url(&hosts, "https://objects.github.com/a").is_err());
        assert!(check_url(&hosts, "https://github.com.attacker.net/a").is_err());
        assert!(check_url(&hosts, "https://notgithub.com/a").is_err());
    }

    #[test]
    fn a_grant_naming_a_port_matches_that_port_only() {
        let hosts = vec!["localhost:8080".to_string()];
        assert!(check_url(&hosts, "http://localhost:8080/a").is_ok());
        assert!(check_url(&hosts, "http://localhost:9090/a").is_err());
        assert!(check_url(&hosts, "http://localhost/a").is_err());
        // …and one without matches any.
        let any = vec!["localhost".to_string()];
        assert!(check_url(&any, "http://localhost:9090/a").is_ok());
    }

    #[test]
    fn plain_http_is_refused_off_loopback() {
        let hosts = vec!["example.com".to_string()];
        let err = check_url(&hosts, "http://example.com/a").expect_err("not https");
        assert!(err.contains("not https"), "{err}");
        assert!(check_url(&hosts, "ftp://example.com/a").is_err());
        assert!(check_url(&hosts, "not a url").is_err());
        assert!(check_url(&hosts, "/just/a/path").is_err());
    }

    #[test]
    fn a_destination_outside_the_write_grant_is_refused() {
        let granted = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let err = refused(
            &grant("github.com", granted.path()),
            "https://github.com/x",
            &sha(BODY),
            &other.path().join("x"),
        );
        assert!(err.contains("denied"), "{err}");
        assert!(err.contains("writable paths"), "{err}");
    }

    /// `fs:read` over the destination is not enough to write there.
    #[test]
    fn a_read_grant_does_not_permit_a_download() {
        let dir = tempfile::tempdir().unwrap();
        let mut g = grant("github.com", dir.path());
        g.fs[0].write = false;
        let err = refused(
            &g,
            "https://github.com/x",
            &sha(BODY),
            &dir.path().join("x"),
        );
        assert!(err.contains("denied"), "{err}");
    }

    #[test]
    fn a_malformed_digest_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let g = grant("github.com", dir.path());
        for bad in ["", "abc", &"z".repeat(64), &"a".repeat(63)] {
            let err = refused(&g, "https://github.com/x", bad, &dir.path().join("x"));
            assert!(err.contains("SHA-256"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn redirects_resolve_absolute_urls_and_paths_only() {
        assert_eq!(
            resolve_redirect("https://a.example/x/y", "https://b.example/z").unwrap(),
            "https://b.example/z"
        );
        assert_eq!(
            resolve_redirect("https://a.example:8443/x/y", "/z?q=1").unwrap(),
            "https://a.example:8443/z?q=1"
        );
        assert!(resolve_redirect("https://a.example/x/y", "z").is_err());
    }
}
