use crate::telemetry::MissionTelemetry;
use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use reqwest::{
    dns::{Addrs, Name, Resolve, Resolving},
    header::{HeaderMap, HeaderName, HeaderValue},
    redirect, Client, Method,
};
use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};
use tower::{Layer, Service};

/// HTTP verbs the cockpit can fly.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl HttpMethod {
    pub const ALL: [HttpMethod; 7] = [
        HttpMethod::Get,
        HttpMethod::Post,
        HttpMethod::Put,
        HttpMethod::Patch,
        HttpMethod::Delete,
        HttpMethod::Head,
        HttpMethod::Options,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            HttpMethod::Get => "GET",
            HttpMethod::Post => "POST",
            HttpMethod::Put => "PUT",
            HttpMethod::Patch => "PATCH",
            HttpMethod::Delete => "DELETE",
            HttpMethod::Head => "HEAD",
            HttpMethod::Options => "OPTIONS",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        HttpMethod::ALL
            .into_iter()
            .find(|m| m.as_str().eq_ignore_ascii_case(s.trim()))
    }

    pub fn next(&self) -> Self {
        let idx = HttpMethod::ALL.iter().position(|m| m == self).unwrap_or(0);
        HttpMethod::ALL[(idx + 1) % HttpMethod::ALL.len()]
    }

    pub fn prev(&self) -> Self {
        let idx = HttpMethod::ALL.iter().position(|m| m == self).unwrap_or(0);
        HttpMethod::ALL[(idx + HttpMethod::ALL.len() - 1) % HttpMethod::ALL.len()]
    }

    /// Whether a request body is meaningful for this verb.
    pub fn allows_body(&self) -> bool {
        matches!(
            self,
            HttpMethod::Post | HttpMethod::Put | HttpMethod::Patch | HttpMethod::Delete
        )
    }

    fn to_reqwest(self) -> Method {
        match self {
            HttpMethod::Get => Method::GET,
            HttpMethod::Post => Method::POST,
            HttpMethod::Put => Method::PUT,
            HttpMethod::Patch => Method::PATCH,
            HttpMethod::Delete => Method::DELETE,
            HttpMethod::Head => Method::HEAD,
            HttpMethod::Options => Method::OPTIONS,
        }
    }
}

/// Records connection-establishment timings out of the connector stack.
///
/// The cockpit keeps at most one request in flight, so a generation counter is
/// enough to guarantee that a late connector callback from an aborted request
/// can never be attributed to the current one.
#[derive(Debug, Default)]
struct Recorder {
    generation: AtomicU64,
    dns_nanos: AtomicU64,
    connect_nanos: AtomicU64,
    dns_seen: AtomicBool,
    connect_seen: AtomicBool,
}

impl Recorder {
    fn begin(&self) -> u64 {
        self.dns_seen.store(false, Ordering::SeqCst);
        self.connect_seen.store(false, Ordering::SeqCst);
        self.dns_nanos.store(0, Ordering::SeqCst);
        self.connect_nanos.store(0, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    fn current(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn record(&self, gen: u64, seen: &AtomicBool, slot: &AtomicU64, d: Duration) {
        if gen != self.current() {
            return; // Stale: belongs to a request that was cancelled.
        }
        // Only the first connection of a request counts; redirects that open
        // further connections are folded into total time, not the handshake row.
        if seen
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            slot.store(d.as_nanos().min(u64::MAX as u128) as u64, Ordering::SeqCst);
        }
    }

    fn read(&self, gen: u64) -> (Option<Duration>, Option<Duration>) {
        if gen != self.current() {
            return (None, None);
        }
        let dns = self
            .dns_seen
            .load(Ordering::SeqCst)
            .then(|| Duration::from_nanos(self.dns_nanos.load(Ordering::SeqCst)));
        let connect = self
            .connect_seen
            .load(Ordering::SeqCst)
            .then(|| Duration::from_nanos(self.connect_nanos.load(Ordering::SeqCst)));
        (dns, connect)
    }
}

/// DNS resolver that times `getaddrinfo` so the waterfall can show name
/// resolution separately from the TCP/TLS handshake.
struct TimingResolver {
    rec: Arc<Recorder>,
}

impl Resolve for TimingResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let rec = self.rec.clone();
        let gen = rec.current();
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let start = Instant::now();
            // Port 0 is a placeholder; reqwest substitutes the real one.
            let addrs = tokio::net::lookup_host((host, 0u16)).await?;
            rec.record(gen, &rec.dns_seen, &rec.dns_nanos, start.elapsed());
            Ok(Box::new(addrs) as Addrs)
        })
    }
}

/// Tower layer that times the whole connect (DNS + TCP + TLS).
#[derive(Clone)]
struct ConnectTimingLayer {
    rec: Arc<Recorder>,
}

impl<S> Layer<S> for ConnectTimingLayer {
    type Service = ConnectTiming<S>;
    fn layer(&self, inner: S) -> Self::Service {
        ConnectTiming {
            inner,
            rec: self.rec.clone(),
        }
    }
}

#[derive(Clone)]
struct ConnectTiming<S> {
    inner: S,
    rec: Arc<Recorder>,
}

impl<S, Req> Service<Req> for ConnectTiming<S>
where
    S: Service<Req>,
    S::Future: Send + 'static,
    S::Response: Send + 'static,
    S::Error: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut TaskContext<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Req) -> Self::Future {
        let fut = self.inner.call(req);
        let rec = self.rec.clone();
        let gen = rec.current();
        let start = Instant::now();
        Box::pin(async move {
            let out = fut.await;
            if out.is_ok() {
                rec.record(gen, &rec.connect_seen, &rec.connect_nanos, start.elapsed());
            }
            out
        })
    }
}

/// Tunables that shape every flight.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub timeout: Duration,
    pub connect_timeout: Duration,
    /// Skip TLS certificate verification. Off by default; opt-in only.
    pub insecure: bool,
    pub follow_redirects: bool,
    pub max_redirects: usize,
    /// Hard ceiling on retained response bytes; the rest is dropped and flagged.
    pub max_body_bytes: u64,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            connect_timeout: Duration::from_secs(10),
            insecure: false,
            follow_redirects: true,
            max_redirects: 10,
            max_body_bytes: 32 * 1024 * 1024,
        }
    }
}

/// Everything a completed flight brings home.
pub struct MissionResult {
    pub telemetry: MissionTelemetry,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub content_type: Option<String>,
    pub final_url: String,
    /// Set when the body hit `max_body_bytes` and was cut short.
    pub truncated: bool,
    /// Non-fatal problems worth telling the pilot about (bad header lines...).
    pub warnings: Vec<String>,
}

/// Summarises the body instead of dumping megabytes into a log or a failed
/// assertion.
impl std::fmt::Debug for MissionResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MissionResult")
            .field("status", &self.telemetry.status)
            .field("final_url", &self.final_url)
            .field("body_bytes", &self.body.len())
            .field("content_type", &self.content_type)
            .field("truncated", &self.truncated)
            .field("warnings", &self.warnings)
            .finish()
    }
}

#[derive(Clone)]
pub struct NetworkEngine {
    client: Client,
    rec: Arc<Recorder>,
    config: EngineConfig,
}

impl NetworkEngine {
    pub fn new(config: EngineConfig) -> Result<Self> {
        let rec = Arc::new(Recorder::default());

        let redirect_policy = if config.follow_redirects {
            redirect::Policy::limited(config.max_redirects)
        } else {
            redirect::Policy::none()
        };

        let client = Client::builder()
            .user_agent(concat!("hcp/", env!("CARGO_PKG_VERSION")))
            .timeout(config.timeout)
            .connect_timeout(config.connect_timeout)
            .redirect(redirect_policy)
            .cookie_store(true)
            .danger_accept_invalid_certs(config.insecure)
            .dns_resolver(Arc::new(TimingResolver { rec: rec.clone() }))
            .connector_layer(ConnectTimingLayer { rec: rec.clone() })
            .build()
            .context("failed to build the HTTP client")?;

        Ok(Self {
            client,
            rec,
            config,
        })
    }

    pub fn config(&self) -> &EngineConfig {
        &self.config
    }

    pub async fn execute_mission(
        &self,
        method: HttpMethod,
        url: &str,
        body: Option<String>,
        raw_headers: &str,
    ) -> Result<MissionResult> {
        let url = normalize_url(url)?;
        let (header_map, mut warnings) = parse_headers(raw_headers);

        let gen = self.rec.begin();
        let t0 = Instant::now();

        let mut request_builder = self
            .client
            .request(method.to_reqwest(), &url)
            .headers(header_map);

        if let Some(payload) = body {
            request_builder = request_builder.body(payload);
        }

        let response = request_builder
            .send()
            .await
            .map_err(|e| anyhow!(describe_error(&e)))?;

        let t_headers = t0.elapsed();
        let (dns, connect) = self.rec.read(gen);
        let handshake = connect.unwrap_or_default();

        let status = response.status().as_u16();
        let http_version = format!("{:?}", response.version());
        let remote_addr = response.remote_addr().map(|a| a.to_string());
        let final_url = response.url().to_string();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_string(),
                    v.to_str().unwrap_or("<binary header value>").to_string(),
                )
            })
            .collect();
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());

        let declared_length = response.content_length();
        let capacity = response
            .content_length()
            .unwrap_or(0)
            .min(self.config.max_body_bytes)
            .min(8 * 1024 * 1024) as usize;
        let mut full_body: Vec<u8> = Vec::with_capacity(capacity);
        let mut size_bytes: u64 = 0;
        let mut truncated = false;

        let mut stream = response.bytes_stream();
        let t_transfer_start = Instant::now();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| anyhow!("transfer interrupted: {}", describe_error(&e)))?;
            let room = self.config.max_body_bytes.saturating_sub(full_body.len() as u64) as usize;
            if chunk.len() >= room {
                full_body.extend_from_slice(&chunk[..room]);
                size_bytes += room as u64;
                truncated = true;
                // Stop pulling bytes we would only throw away: honouring the
                // cap must also cap what crosses the network.
                break;
            }
            size_bytes += chunk.len() as u64;
            full_body.extend_from_slice(&chunk);
        }

        let transfer = t_transfer_start.elapsed();
        let total = t0.elapsed();
        // `send()` covers connect + request + first byte; the server's share is
        // whatever is left once the handshake is subtracted.
        let server = t_headers.saturating_sub(handshake);

        if truncated {
            let declared = declared_length
                .map(|n| format!(" The server declared {}.", crate::telemetry::fmt_bytes(n)))
                .unwrap_or_default();
            warnings.push(format!(
                "Response hit the {} MB limit and was truncated; the rest was not \
                 downloaded.{declared}",
                self.config.max_body_bytes / (1024 * 1024)
            ));
        }

        Ok(MissionResult {
            telemetry: MissionTelemetry {
                dns,
                connect,
                server,
                transfer,
                total,
                size_bytes,
                status,
                reused_connection: connect.is_none(),
                http_version,
                remote_addr,
            },
            headers,
            body: full_body,
            content_type,
            final_url,
            truncated,
            warnings,
        })
    }
}

/// Accepts what a human types. `example.com/api` becomes a real URL instead of
/// a "relative URL without a base" error.
pub fn normalize_url(input: &str) -> Result<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("no target URL — type an endpoint in the URL bar"));
    }
    if trimmed.contains("://") {
        return Ok(trimmed.to_string());
    }
    if trimmed.starts_with("//") {
        return Ok(format!("https:{trimmed}"));
    }
    // Bare host or localhost:port — assume plaintext for loopback, TLS elsewhere.
    let host = trimmed.split(['/', '?', '#']).next().unwrap_or(trimmed);
    let bare_host = host.split(':').next().unwrap_or(host);
    let is_local = bare_host.eq_ignore_ascii_case("localhost")
        || bare_host == "127.0.0.1"
        || bare_host == "0.0.0.0"
        || bare_host == "[::1]";
    Ok(format!(
        "{}://{}",
        if is_local { "http" } else { "https" },
        trimmed
    ))
}

/// Parses `Key: Value` lines, skipping blanks and `#` comments, and reporting
/// every line it could not use instead of dropping it silently.
pub fn parse_headers(raw: &str) -> (HeaderMap, Vec<String>) {
    let mut map = HeaderMap::new();
    let mut warnings = Vec::new();

    for (idx, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let Some((k, v)) = line.split_once(':') else {
            warnings.push(format!("Header line {}: missing ':' — ignored.", idx + 1));
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        if key.is_empty() {
            warnings.push(format!("Header line {}: empty name — ignored.", idx + 1));
            continue;
        }
        match (HeaderName::from_str(key), HeaderValue::from_str(val)) {
            (Ok(name), Ok(value)) => {
                // Repeating a name appends, matching how HTTP multi-value
                // headers (Set-Cookie, Accept) actually work.
                map.append(name, value);
            }
            (Err(_), _) => warnings.push(format!("Header line {}: invalid name '{}'.", idx + 1, key)),
            (_, Err(_)) => warnings.push(format!("Header line {}: invalid value for '{}'.", idx + 1, key)),
        }
    }

    (map, warnings)
}

/// Turns reqwest's terse errors into something a pilot can act on.
fn describe_error(e: &reqwest::Error) -> String {
    // Order matters: a connect timeout reports as both a connect error and a
    // timeout, and "could not connect" is far more actionable.
    let class = if e.is_connect() && e.is_timeout() {
        "could not connect — the connection attempt timed out"
    } else if e.is_connect() {
        "could not establish a connection"
    } else if e.is_timeout() {
        "request timed out"
    } else if e.is_redirect() {
        "too many redirects"
    } else if e.is_decode() {
        "could not decode the response body"
    } else if e.is_builder() {
        "malformed request"
    } else {
        "request failed"
    };

    // The innermost source carries the real cause ("Connection refused",
    // "certificate has expired"); the wrappers above it are library noise.
    let mut deepest: Option<String> = None;
    let mut source = std::error::Error::source(e);
    while let Some(s) = source {
        let text = s.to_string();
        if !text.is_empty() {
            deepest = Some(text);
        }
        source = std::error::Error::source(s);
    }

    compose_error(class, deepest.as_deref(), &e.to_string())
}

/// Pure assembly step, kept separate so it can be tested without conjuring a
/// `reqwest::Error`.
fn compose_error(class: &str, deepest: Option<&str>, fallback: &str) -> String {
    let detail = deepest.filter(|d| !adds_nothing(class, d));
    let mut out = match detail {
        Some(d) => format!("{class} — {d}"),
        None if class == "request failed" => format!("{class} — {fallback}"),
        None => class.to_string(),
    };

    let lower = out.to_ascii_lowercase();
    if lower.contains("certificate") || lower.contains("self-signed") {
        out.push_str(
            "\n\nHint: pass --insecure to skip TLS verification for a self-signed endpoint.",
        );
    } else if lower.contains("lookup address") || lower.contains("name or service not known") {
        out.push_str("\n\nHint: check the hostname, or your DNS/VPN settings.");
    }
    out
}

/// True when the detail only restates the classification.
fn adds_nothing(class: &str, detail: &str) -> bool {
    let d = detail.to_ascii_lowercase();
    if class.contains("timed out") && (d.contains("deadline has elapsed") || d.contains("timed out"))
    {
        return true;
    }
    d.is_empty() || d.starts_with("client error") || d.starts_with("error sending request")
}

/// Canonical reason phrase for a status code, e.g. 404 -> "Not Found".
pub fn status_reason(code: u16) -> &'static str {
    reqwest::StatusCode::from_u16(code)
        .ok()
        .and_then(|s| s.canonical_reason())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_normalization() {
        assert_eq!(normalize_url("https://a.com").unwrap(), "https://a.com");
        assert_eq!(normalize_url(" example.com/api ").unwrap(), "https://example.com/api");
        assert_eq!(normalize_url("localhost:8080/x").unwrap(), "http://localhost:8080/x");
        assert_eq!(normalize_url("127.0.0.1:3000").unwrap(), "http://127.0.0.1:3000");
        assert_eq!(normalize_url("//cdn.com/a").unwrap(), "https://cdn.com/a");
        assert!(normalize_url("   ").is_err());
    }

    #[test]
    fn header_parsing_reports_problems_instead_of_swallowing_them() {
        let (map, warns) = parse_headers(
            "Content-Type: application/json\n\
             # a comment\n\
             \n\
             broken line\n\
             : novalue\n\
             Authorization: Bearer abc",
        );
        assert_eq!(map.get("content-type").unwrap(), "application/json");
        assert_eq!(map.get("authorization").unwrap(), "Bearer abc");
        assert_eq!(warns.len(), 2, "malformed lines must surface: {warns:?}");
    }

    #[test]
    fn repeated_header_names_append() {
        let (map, _) = parse_headers("Accept: text/html\nAccept: application/json");
        assert_eq!(map.get_all("accept").iter().count(), 2);
    }

    #[test]
    fn header_values_with_colons_survive() {
        let (map, warns) = parse_headers("X-Target: https://example.com:8443/path");
        assert!(warns.is_empty());
        assert_eq!(map.get("x-target").unwrap(), "https://example.com:8443/path");
    }

    #[test]
    fn error_messages_stay_short_and_specific() {
        assert_eq!(
            compose_error(
                "could not connect — the connection attempt timed out",
                Some("deadline has elapsed"),
                "x"
            ),
            "could not connect — the connection attempt timed out",
            "the detail must not merely restate the class"
        );
        assert_eq!(
            compose_error(
                "could not establish a connection",
                Some("Connection refused (os error 111)"),
                "x"
            ),
            "could not establish a connection — Connection refused (os error 111)"
        );
        assert!(compose_error("request failed", None, "raw text").contains("raw text"));
    }

    #[test]
    fn tls_and_dns_failures_come_with_a_next_step() {
        let tls = compose_error("request failed", Some("invalid peer certificate"), "");
        assert!(tls.contains("--insecure"), "{tls}");
        let dns = compose_error(
            "could not establish a connection",
            Some("failed to lookup address information: Name or service not known"),
            "",
        );
        assert!(dns.contains("DNS"), "{dns}");
    }

    #[test]
    fn library_wrapper_text_is_filtered_out() {
        assert!(adds_nothing("request failed", "client error (Connect)"));
        assert!(!adds_nothing("request failed", "Connection refused"));
    }

    #[test]
    fn method_cycling_wraps_both_ways() {
        assert_eq!(HttpMethod::Get.prev(), HttpMethod::Options);
        assert_eq!(HttpMethod::Options.next(), HttpMethod::Get);
        assert_eq!(HttpMethod::parse("patch"), Some(HttpMethod::Patch));
        assert_eq!(HttpMethod::parse("nope"), None);
    }

    #[test]
    fn body_verbs() {
        assert!(HttpMethod::Post.allows_body());
        assert!(!HttpMethod::Get.allows_body());
        assert!(!HttpMethod::Head.allows_body());
    }
}
