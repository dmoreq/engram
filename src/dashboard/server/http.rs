//! HTTP request/response primitives: bounded parsing, loopback Host guard,
//! defensive headers, and one stderr line per request.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::Instant;

pub(super) struct HttpRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) body: Vec<u8>,
    pub(super) content_type: String,
    pub(super) origin: Option<String>,
    pub(super) host: Option<String>,
}

const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_HEADERS: usize = 64;
/// Tool-facing POST bodies (JSON commands) never approach this; anything
/// larger is either abuse or a client bug, and both are refused.
const MAX_BODY_BYTES: usize = 4096;

pub(super) fn read_http_request(stream: &mut TcpStream) -> std::io::Result<Option<HttpRequest>> {
    let mut reader = BufReader::new(stream);
    // The request line is bounded like the header block below: a first line
    // over the cap aborts parsing instead of growing the buffer without limit.
    let mut request_line = Vec::new();
    let read = (&mut reader)
        .take(MAX_HEADER_BYTES as u64)
        .read_until(b'\n', &mut request_line)?;
    if read == 0 || request_line.last() != Some(&b'\n') {
        // Empty stream, or a first line over the cap (or torn mid-line):
        // refuse the request.
        return Ok(None);
    }
    let request_line = String::from_utf8(request_line)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;

    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 {
        return Ok(None);
    }

    let mut content_length = 0usize;
    let mut content_type = String::new();
    let mut origin = None;
    let mut host = None;
    let mut line = String::new();
    let mut header_bytes = request_line.len();
    let mut header_count = 0usize;
    // Duplicate routing headers are a request-smuggling primitive: the first
    // value decides the parse here, a proxy may forward the last, so any
    // second Host or Content-Length refuses the request outright.
    let mut saw_host = false;
    let mut saw_content_length = false;
    loop {
        line.clear();
        // Bound each header line read like the request line above: take()
        // stops at the remaining budget so a no-newline header cannot grow
        // the buffer without limit before the byte cap is checked.
        let remaining = (MAX_HEADER_BYTES + 1).saturating_sub(header_bytes) as u64;
        let read = (&mut reader).take(remaining).read_line(&mut line)?;
        if read == 0 || line.trim().is_empty() {
            break;
        }
        if !line.ends_with('\n') {
            // The take() budget ran out mid-line: over the cap, refuse.
            return Ok(None);
        }
        header_bytes += read;
        header_count += 1;
        if header_bytes > MAX_HEADER_BYTES || header_count > MAX_HEADERS {
            return Ok(None);
        }
        if let Some((header_name, header_val)) = line.split_once(':') {
            let name = header_name.trim();
            let val = header_val.trim();
            if name.eq_ignore_ascii_case("content-length") {
                if saw_content_length {
                    return Ok(None);
                }
                saw_content_length = true;
                content_length = val.parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("content-type") {
                content_type = val.to_ascii_lowercase();
            } else if name.eq_ignore_ascii_case("origin") {
                origin = Some(val.to_string());
            } else if name.eq_ignore_ascii_case("host") {
                if saw_host {
                    return Ok(None);
                }
                saw_host = true;
                host = Some(val.to_string());
            }
        }
    }

    let mut body = Vec::new();
    if content_length > MAX_BODY_BYTES {
        return Ok(None);
    }
    if content_length > 0 {
        body.resize(content_length, 0);
        reader.read_exact(&mut body)?;
    }

    Ok(Some(HttpRequest {
        method: parts[0].to_string(),
        path: parts[1].to_string(),
        body,
        content_type,
        origin,
        host,
    }))
}

/// The dashboard answers only for loopback hostnames. A rebound public domain
/// resolves to 127.0.0.1 but still sends its own `Host`, which this rejects.
/// Bracketed IPv6 literals are read between the brackets (RFC 7230), so
/// `[::1]:port` is accepted; bare IPv6 forms (`::1`, `::1:8080`) have no
/// unambiguous hostname and stay rejected.
pub(super) fn host_allowed(host: Option<&str>) -> bool {
    // A missing Host is not a loopback attestation: HTTP/1.1 requires it,
    // and accepting it would let a client skip the DNS-rebinding guard
    // entirely.
    let Some(host) = host else {
        return false;
    };
    let Some(name) = host_name(host) else {
        return false;
    };
    // "0.0.0.0" is deliberately absent: some platforms route a public
    // wildcard destination to loopback (the "0.0.0.0 Day" class), so it is
    // not a loopback attestation.
    matches!(name, "127.0.0.1" | "localhost" | "::1")
}

/// Extract the Host header's hostname: the text between brackets for a
/// bracketed literal, else the text before the first `:`. A bracket without
/// a closing `]`, or a leading empty segment (`::1` splits to `""`), is
/// ambiguous and yields no hostname.
fn host_name(host: &str) -> Option<&str> {
    if let Some(rest) = host.strip_prefix('[') {
        return rest.split_once(']').map(|(inside, _)| inside);
    }
    let name = host.split(':').next().unwrap_or(host);
    if name.is_empty() {
        return None;
    }
    Some(name)
}

/// Loopback-only request authenticity: a mutating request must be
/// `application/json`, and a browser-supplied `Origin` must match the Host it
/// addressed. Non-browser clients (curl) may omit Origin.
pub(super) fn touch_request_allowed(req: &HttpRequest) -> std::result::Result<(), (u16, String)> {
    if !req.content_type.starts_with("application/json") {
        return Err((415, "Content-Type must be application/json".to_string()));
    }
    if let Some(origin) = &req.origin {
        let expected = req
            .host
            .as_deref()
            .map(|h| format!("http://{h}"))
            .unwrap_or_default();
        if origin != &expected {
            return Err((403, "cross-origin mutation rejected".to_string()));
        }
    }
    Ok(())
}

pub(super) struct RequestLog {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) started: Instant,
}

impl Drop for RequestLog {
    fn drop(&mut self) {
        // Sanitize: a raw request path can carry ANSI escapes that forge
        // terminal log lines.
        eprintln!(
            "eg dashboard: {} {} {:.1}ms",
            crate::query::sanitize(&self.method),
            crate::query::sanitize(&self.path),
            self.started.elapsed().as_secs_f64() * 1000.0
        );
    }
}

/// Send one response with the dashboard's fixed defensive headers. Unknown
/// statuses are 500s, never mislabelled successes.
pub(super) fn send_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    is_head: bool,
) -> std::io::Result<()> {
    let status_line = match status {
        200 => "200 OK",
        400 => "400 Bad Request",
        403 => "403 Forbidden",
        404 => "404 Not Found",
        405 => "405 Method Not Allowed",
        409 => "409 Conflict",
        415 => "415 Unsupported Media Type",
        503 => "503 Service Unavailable",
        _ => "500 Internal Server Error",
    };
    let header = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: {content_type}\r\nCache-Control: no-store\r\n\
         Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n\
         X-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    if !is_head {
        stream.write_all(body)?;
    }
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_allowed_accepts_loopback_forms() {
        for host in [
            "[::1]:8080",
            "127.0.0.1:8080",
            "localhost:3000",
            "localhost",
        ] {
            assert!(host_allowed(Some(host)), "{host} must be accepted");
        }
    }

    #[test]
    fn host_allowed_rejects_public_hosts_and_bare_ipv6_forms() {
        for host in [
            "evil.example:80",
            "evil.example",
            "[2001:db8::1]:443",
            "::1",
            "::1:8080",
            // Wildcard destinations are not loopback attestations (0.0.0.0 Day).
            "0.0.0.0:80",
            "0.0.0.0",
        ] {
            assert!(!host_allowed(Some(host)), "{host} must be rejected");
        }
    }

    #[test]
    fn request_line_over_cap_is_rejected() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        let big = format!("GET /{}.html HTTP/1.1\r\n", "a".repeat(9000));
        std::io::Write::write_all(&mut client, big.as_bytes()).unwrap();
        drop(client);
        let (mut server, _) = listener.accept().unwrap();
        assert!(read_http_request(&mut server).unwrap().is_none());
    }

    #[test]
    fn request_line_at_the_cap_still_parses() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let head = "GET /";
        let tail = " HTTP/1.1\r\n";
        let pad = MAX_HEADER_BYTES - head.len() - tail.len() - ".html".len();
        let line = format!("{head}{}.html{tail}", "a".repeat(pad));
        assert_eq!(line.len(), MAX_HEADER_BYTES);
        let mut client = std::net::TcpStream::connect(addr).unwrap();
        std::io::Write::write_all(&mut client, line.as_bytes()).unwrap();
        drop(client);
        let (mut server, _) = listener.accept().unwrap();
        let req = read_http_request(&mut server).unwrap();
        assert!(req.is_some(), "a request line of exactly the cap parses");
        assert_eq!(req.unwrap().path.len(), MAX_HEADER_BYTES - 4 - tail.len());
    }

    #[test]
    fn duplicate_routing_headers_are_rejected() {
        for headers in [
            "Host: 127.0.0.1:8080\r\nHost: 127.0.0.1:8080\r\n",
            "Content-Length: 2\r\nContent-Length: 2\r\n",
        ] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let raw = format!("POST / HTTP/1.1\r\n{headers}Connection: close\r\n\r\n");
            let mut client = std::net::TcpStream::connect(addr).unwrap();
            std::io::Write::write_all(&mut client, raw.as_bytes()).unwrap();
            drop(client);
            let (mut server, _) = listener.accept().unwrap();
            assert!(
                read_http_request(&mut server).unwrap().is_none(),
                "duplicate {headers:?} must refuse the request"
            );
        }
    }
}
