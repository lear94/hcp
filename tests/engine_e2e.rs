//! End-to-end tests for the network engine against a real HTTP server.
//!
//! These cover the behaviour that unit tests cannot: phase timing, connection
//! reuse, redirects, truncation and timeouts all need real sockets.

use hcp::engine::{EngineConfig, HttpMethod, NetworkEngine};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// A deliberately small HTTP/1.1 server: enough to exercise the client, with
/// no dependency on an external service (and therefore no flaky tests).
async fn spawn_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(handle_conn(stream));
        }
    });
    addr
}

async fn handle_conn(mut stream: TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];

    // Keep-alive: serve requests off the same socket until the peer goes away.
    loop {
        let head_end = loop {
            if let Some(pos) = find_double_crlf(&buf) {
                break pos;
            }
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };

        let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default().to_string();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or("GET").to_string();
        let path = parts.next().unwrap_or("/").to_string();

        let mut headers = Vec::new();
        let mut content_length = 0usize;
        for line in lines {
            if let Some((k, v)) = line.split_once(':') {
                if k.trim().eq_ignore_ascii_case("content-length") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
                headers.push((k.trim().to_string(), v.trim().to_string()));
            }
        }

        buf.drain(..head_end + 4);
        while buf.len() < content_length {
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let body: Vec<u8> = buf.drain(..content_length).collect();

        let response = route(&method, &path, &headers, &body).await;
        if stream.write_all(&response).await.is_err() {
            return;
        }
    }
}

fn find_double_crlf(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

async fn route(
    method: &str,
    path: &str,
    headers: &[(String, String)],
    body: &[u8],
) -> Vec<u8> {
    let (query_path, query) = match path.split_once('?') {
        Some((p, q)) => (p, q),
        None => (path, ""),
    };

    match query_path {
        "/ok" => text_response(200, "application/json", br#"{"hello":"world","n":1}"#, method),
        "/echo" => {
            let mut out = String::from("{\"headers\":{");
            let mut first = true;
            for (k, v) in headers {
                if k.eq_ignore_ascii_case("host") || k.eq_ignore_ascii_case("connection") {
                    continue;
                }
                if !first {
                    out.push(',');
                }
                first = false;
                out.push_str(&format!("\"{}\":\"{}\"", k.to_lowercase(), v));
            }
            out.push_str("},\"body\":");
            out.push_str(&format!("\"{}\"", String::from_utf8_lossy(body)));
            out.push('}');
            text_response(200, "application/json", out.as_bytes(), method)
        }
        "/big" => {
            let n: usize = query
                .strip_prefix("n=")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1024);
            text_response(200, "text/plain", &vec![b'a'; n], method)
        }
        "/redirect" => {
            let mut out = Vec::new();
            out.extend_from_slice(
                b"HTTP/1.1 302 Found\r\nLocation: /ok\r\nContent-Length: 0\r\n\r\n",
            );
            out
        }
        "/slow" => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            text_response(200, "text/plain", b"late", method)
        }
        "/binary" => text_response(200, "application/octet-stream", &[0u8, 1, 2, 3, 0, 255], method),
        "/notfound" => text_response(404, "text/plain", b"nope", method),
        "/multi" => {
            let mut out = Vec::new();
            out.extend_from_slice(
                b"HTTP/1.1 200 OK\r\nSet-Cookie: a=1\r\nSet-Cookie: b=2\r\nContent-Type: text/plain\r\nContent-Length: 2\r\n\r\nhi",
            );
            out
        }
        _ => text_response(404, "text/plain", b"unknown route", method),
    }
}

fn text_response(status: u16, content_type: &str, body: &[u8], method: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        302 => "Found",
        404 => "Not Found",
        _ => "Status",
    };
    let mut out = Vec::new();
    out.extend_from_slice(
        format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nX-Test: cockpit\r\n\r\n",
            body.len()
        )
        .as_bytes(),
    );
    // HEAD must carry the headers of a GET but none of the body.
    if method != "HEAD" {
        out.extend_from_slice(body);
    }
    out
}

fn engine(config: EngineConfig) -> NetworkEngine {
    NetworkEngine::new(config).expect("engine builds")
}

#[tokio::test]
async fn get_returns_status_headers_body_and_timings() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    let r = e
        .execute_mission(HttpMethod::Get, &format!("http://{addr}/ok"), None, "")
        .await
        .expect("request succeeds");

    assert_eq!(r.telemetry.status, 200);
    assert_eq!(String::from_utf8_lossy(&r.body), r#"{"hello":"world","n":1}"#);
    assert_eq!(r.telemetry.size_bytes, r.body.len() as u64);
    assert_eq!(r.content_type.as_deref(), Some("application/json"));
    assert!(r
        .headers
        .iter()
        .any(|(k, v)| k.eq_ignore_ascii_case("x-test") && v == "cockpit"));
    assert!(r.telemetry.total > Duration::ZERO);
    assert!(!r.truncated);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
}

#[tokio::test]
async fn the_first_request_measures_dns_and_connect_separately() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    let r = e
        .execute_mission(HttpMethod::Get, &format!("http://{addr}/ok"), None, "")
        .await
        .unwrap();

    let connect = r.telemetry.connect.expect("a fresh connection was opened");
    assert!(!r.telemetry.reused_connection);
    // The host is a literal address, so no resolver runs — and the waterfall
    // must say so rather than pretending the lookup took zero time.
    assert!(r.telemetry.dns.is_none());
    assert!(r.telemetry.phases()[0].note.contains("literal IP"));
    assert!(
        r.telemetry.total >= connect,
        "total must contain every phase"
    );
    assert!(r.telemetry.server > Duration::ZERO, "server phase must be measured");
}

#[tokio::test]
async fn a_pooled_connection_reports_itself_as_reused() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    let url = format!("http://{addr}/ok");

    let first = e
        .execute_mission(HttpMethod::Get, &url, None, "")
        .await
        .unwrap();
    assert!(!first.telemetry.reused_connection);

    let second = e
        .execute_mission(HttpMethod::Get, &url, None, "")
        .await
        .unwrap();
    assert!(
        second.telemetry.reused_connection,
        "the second request should ride the pooled connection"
    );
    assert!(second.telemetry.connect.is_none());
    assert!(second.telemetry.dns.is_none());
}

#[tokio::test]
async fn headers_and_body_reach_the_server() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    let r = e
        .execute_mission(
            HttpMethod::Post,
            &format!("http://{addr}/echo"),
            Some(r#"{"ping":true}"#.to_string()),
            "Content-Type: application/json\nX-Trace: abc123\n# a comment\n\n",
        )
        .await
        .unwrap();

    let text = String::from_utf8_lossy(&r.body);
    assert!(text.contains("\"x-trace\":\"abc123\""), "{text}");
    assert!(text.contains("\"content-type\":\"application/json\""), "{text}");
    assert!(text.contains(r#"{\"ping\":true}"#) || text.contains("ping"), "{text}");
}

#[tokio::test]
async fn malformed_header_lines_are_reported_not_swallowed() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    let r = e
        .execute_mission(
            HttpMethod::Get,
            &format!("http://{addr}/ok"),
            None,
            "Good: yes\nthis line has no colon",
        )
        .await
        .unwrap();
    assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
    assert!(r.warnings[0].contains("missing ':'"));
}

#[tokio::test]
async fn redirects_are_followed_and_the_final_url_is_reported() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    let r = e
        .execute_mission(HttpMethod::Get, &format!("http://{addr}/redirect"), None, "")
        .await
        .unwrap();
    assert_eq!(r.telemetry.status, 200);
    assert!(r.final_url.ends_with("/ok"), "{}", r.final_url);
}

#[tokio::test]
async fn redirects_can_be_switched_off() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig {
        follow_redirects: false,
        ..EngineConfig::default()
    });
    let r = e
        .execute_mission(HttpMethod::Get, &format!("http://{addr}/redirect"), None, "")
        .await
        .unwrap();
    assert_eq!(r.telemetry.status, 302);
}

#[tokio::test]
async fn oversized_responses_are_truncated_and_flagged() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig {
        max_body_bytes: 1024,
        ..EngineConfig::default()
    });
    let r = e
        .execute_mission(
            HttpMethod::Get,
            &format!("http://{addr}/big?n=100000"),
            None,
            "",
        )
        .await
        .unwrap();

    assert!(r.truncated);
    assert_eq!(r.body.len(), 1024, "retained bytes must respect the cap");
    assert_eq!(
        r.telemetry.size_bytes, 1024,
        "the cap must also stop the download, not just the display"
    );
    let warning = r.warnings.join(" ");
    assert!(warning.contains("truncated"), "{warning}");
    assert!(
        warning.contains("97.7 KB"),
        "the declared size belongs in the warning: {warning}"
    );
}

#[tokio::test]
async fn a_timeout_produces_a_readable_error() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig {
        timeout: Duration::from_millis(300),
        ..EngineConfig::default()
    });
    let err = e
        .execute_mission(HttpMethod::Get, &format!("http://{addr}/slow"), None, "")
        .await
        .expect_err("the slow route must time out");
    let text = format!("{err:#}");
    assert!(text.contains("timed out"), "{text}");
}

#[tokio::test]
async fn a_refused_connection_explains_itself() {
    // Port 1 on loopback is reliably closed.
    let e = engine(EngineConfig {
        connect_timeout: Duration::from_millis(500),
        ..EngineConfig::default()
    });
    let err = e
        .execute_mission(HttpMethod::Get, "http://127.0.0.1:1/", None, "")
        .await
        .expect_err("connecting must fail");
    let text = format!("{err:#}");
    assert!(
        text.contains("connect"),
        "a closed port must be reported as a connection problem, not a bare \
         timeout: {text}"
    );
}

#[tokio::test]
async fn head_requests_carry_headers_without_a_body() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    let r = e
        .execute_mission(HttpMethod::Head, &format!("http://{addr}/ok"), None, "")
        .await
        .unwrap();
    assert_eq!(r.telemetry.status, 200);
    assert!(r.body.is_empty());
    assert!(!r.headers.is_empty());
}

#[tokio::test]
async fn error_statuses_are_delivered_not_treated_as_failures() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    let r = e
        .execute_mission(HttpMethod::Get, &format!("http://{addr}/notfound"), None, "")
        .await
        .expect("a 404 is a response, not a transport error");
    assert_eq!(r.telemetry.status, 404);
    assert_eq!(String::from_utf8_lossy(&r.body), "nope");
}

#[tokio::test]
async fn repeated_response_headers_are_all_preserved() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    let r = e
        .execute_mission(HttpMethod::Get, &format!("http://{addr}/multi"), None, "")
        .await
        .unwrap();
    let cookies: Vec<_> = r
        .headers
        .iter()
        .filter(|(k, _)| k.eq_ignore_ascii_case("set-cookie"))
        .collect();
    assert_eq!(cookies.len(), 2, "{:?}", r.headers);
}

#[tokio::test]
async fn a_bare_host_is_normalised_into_a_usable_url() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    // No scheme, loopback host: must become http:// and just work.
    let r = e
        .execute_mission(HttpMethod::Get, &format!("127.0.0.1:{}/ok", addr.port()), None, "")
        .await
        .unwrap();
    assert_eq!(r.telemetry.status, 200);
}

#[tokio::test]
async fn an_empty_url_fails_before_touching_the_network() {
    let e = engine(EngineConfig::default());
    let err = e
        .execute_mission(HttpMethod::Get, "   ", None, "")
        .await
        .expect_err("an empty target must be rejected");
    assert!(format!("{err:#}").contains("no target URL"));
}

#[tokio::test]
async fn binary_payloads_survive_the_round_trip() {
    let addr = spawn_server().await;
    let e = engine(EngineConfig::default());
    let r = e
        .execute_mission(HttpMethod::Get, &format!("http://{addr}/binary"), None, "")
        .await
        .unwrap();
    assert_eq!(r.body, vec![0u8, 1, 2, 3, 0, 255]);
}
