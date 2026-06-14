// Hand-written HTTP/1.1 parser and serializer — no external HTTP library.

use std::io::{self, BufRead, Read, Write};

// Hard cap on the request/response head (request line + headers + blank line).
pub const MAX_HEAD_BYTES: usize = 16 * 1024;

// Hard cap on body size; large enough to serve PDF textbooks buffered in memory.
pub const MAX_BODY_BYTES: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct Request {
    pub method: String,
    pub target: String,
    pub version: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    // Build a 200 OK with Content-Type, Content-Length, Connection: close, and Cache-Control: no-store.
    pub fn ok(body: Vec<u8>, content_type: &str) -> Self {
        let mut headers = vec![
            ("Content-Type".into(), content_type.into()),
            ("Content-Length".into(), body.len().to_string()),
            ("Connection".into(), "close".into()),
        ];
        headers.push(("Cache-Control".into(), "no-store".into()));
        Self {
            status: 200,
            reason: "OK".into(),
            headers,
            body,
        }
    }

    // Build a response with any status code and a plain-text body.
    pub fn status(code: u16, reason: &str, body: &str) -> Self {
        let body = body.as_bytes().to_vec();
        let headers = vec![
            ("Content-Type".into(), "text/plain; charset=utf-8".into()),
            ("Content-Length".into(), body.len().to_string()),
            ("Connection".into(), "close".into()),
        ];
        Self {
            status: code,
            reason: reason.into(),
            headers,
            body,
        }
    }

    // Append a header without deduplication.
    pub fn add_header(&mut self, name: &str, value: &str) {
        self.headers.push((name.into(), value.into()));
    }
}

/// Case-insensitive header lookup.
pub fn header_value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// Parse an HTTP request from a buffered reader.
pub fn read_request<R: BufRead>(r: &mut R) -> io::Result<Request> {
    let head = read_head(r)?;
    let mut lines = head.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing request line"))?;
    let mut parts = request_line.splitn(3, ' ');
    let method = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing method"))?
        .to_string();
    let target = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing target"))?
        .to_string();
    let version = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing version"))?
        .to_string();

    let headers = parse_headers(lines)?;
    let body = read_body(r, &headers)?;

    Ok(Request {
        method,
        target,
        version,
        headers,
        body,
    })
}

/// Parse an HTTP response from a buffered reader.
pub fn read_response<R: BufRead>(r: &mut R) -> io::Result<Response> {
    let head = read_head(r)?;
    let mut lines = head.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing status line"))?;
    let mut parts = status_line.splitn(3, ' ');
    let _version = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing version"))?;
    let status: u16 = parts
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing status"))?
        .parse()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-numeric status"))?;
    let reason = parts.next().unwrap_or("").to_string();

    let headers = parse_headers(lines)?;
    let body = read_body(r, &headers)?;

    Ok(Response {
        status,
        reason,
        headers,
        body,
    })
}

// Read lines until the blank line that ends an HTTP head section; bounded by MAX_HEAD_BYTES.
fn read_head<R: BufRead>(r: &mut R) -> io::Result<String> {
    let mut buf = Vec::with_capacity(1024);
    loop {
        if buf.len() >= MAX_HEAD_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "request head exceeded MAX_HEAD_BYTES",
            ));
        }
        let before = buf.len();
        let n = r.read_until(b'\n', &mut buf)?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before end of headers",
            ));
        }
        let line = &buf[before..];
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }
    let s = String::from_utf8(buf)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "non-utf8 header section"))?;
    Ok(s.trim_end_matches(['\r', '\n']).to_string())
}

// Split header lines into (name, value) pairs; skips empty lines.
fn parse_headers<'a, I: Iterator<Item = &'a str>>(lines: I) -> io::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "malformed header line"))?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }
    Ok(headers)
}

// Read exactly Content-Length bytes; returns empty body if header is absent.
fn read_body<R: Read>(r: &mut R, headers: &[(String, String)]) -> io::Result<Vec<u8>> {
    let len: usize = match header_value(headers, "Content-Length") {
        Some(v) => v
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad Content-Length"))?,
        None => 0,
    };
    if len > MAX_BODY_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Content-Length exceeds MAX_BODY_BYTES",
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(body)
}

/// Serialize a Response to HTTP/1.1 wire format and flush.
pub fn write_response<W: Write>(w: &mut W, resp: &Response) -> io::Result<()> {
    write!(w, "HTTP/1.1 {} {}\r\n", resp.status, resp.reason)?;
    for (k, v) in &resp.headers {
        write!(w, "{}: {}\r\n", k, v)?;
    }
    w.write_all(b"\r\n")?;
    w.write_all(&resp.body)?;
    w.flush()?;
    Ok(())
}

/// Serialize a Request to HTTP/1.1 wire format and flush.
pub fn write_request<W: Write>(w: &mut W, req: &Request) -> io::Result<()> {
    write!(w, "{} {} {}\r\n", req.method, req.target, req.version)?;
    for (k, v) in &req.headers {
        write!(w, "{}: {}\r\n", k, v)?;
    }
    w.write_all(b"\r\n")?;
    w.write_all(&req.body)?;
    w.flush()?;
    Ok(())
}

// Health report sent as a POST body to the LB's health-ingest port.
// Wire format: key=value lines (server_id, load, available, in_flight).
#[derive(Debug, Clone)]
pub struct HealthReport {
    pub server_id: String,
    pub load: f32,
    pub available: bool,
    pub in_flight: u32,
}

impl HealthReport {
    // Serialize to key=value line format.
    pub fn encode(&self) -> String {
        format!(
            "server_id={}\nload={}\navailable={}\nin_flight={}\n",
            self.server_id, self.load, self.available, self.in_flight
        )
    }

    // Parse key=value lines; returns None if any required field is missing or invalid.
    pub fn decode(s: &str) -> Option<Self> {
        let mut server_id = None;
        let mut load = None;
        let mut available = None;
        let mut in_flight = None;
        for line in s.lines() {
            let (k, v) = line.split_once('=')?;
            match k.trim() {
                "server_id" => server_id = Some(v.trim().to_string()),
                "load" => load = v.trim().parse().ok(),
                "available" => available = Some(v.trim() == "true"),
                "in_flight" => in_flight = v.trim().parse().ok(),
                _ => {}
            }
        }
        Some(HealthReport {
            server_id: server_id?,
            load: load?,
            available: available?,
            in_flight: in_flight?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn parses_simple_get() {
        let raw = b"GET /file HTTP/1.1\r\nHost: lb\r\nX-Auth-Token: abc\r\n\r\n";
        let mut r = Cursor::new(&raw[..]);
        let req = read_request(&mut r).unwrap();
        assert_eq!(req.method, "GET");
        assert_eq!(req.target, "/file");
        assert_eq!(header_value(&req.headers, "host"), Some("lb"));
    }

    #[test]
    fn parses_post_with_body() {
        let raw = b"POST /_health HTTP/1.1\r\nContent-Length: 5\r\n\r\nhello";
        let mut r = Cursor::new(&raw[..]);
        let req = read_request(&mut r).unwrap();
        assert_eq!(req.body, b"hello");
    }

    #[test]
    fn rejects_too_large_head() {
        let mut raw = Vec::from(&b"GET / HTTP/1.1\r\n"[..]);
        let big = "X-Pad: ".to_string() + &"a".repeat(MAX_HEAD_BYTES) + "\r\n";
        raw.extend_from_slice(big.as_bytes());
        raw.extend_from_slice(b"\r\n");
        let mut r = Cursor::new(raw);
        assert!(read_request(&mut r).is_err());
    }

    #[test]
    fn round_trip_health_report() {
        let r = HealthReport {
            server_id: "server3".into(),
            load: 0.42,
            available: true,
            in_flight: 7,
        };
        let parsed = HealthReport::decode(&r.encode()).unwrap();
        assert_eq!(parsed.server_id, "server3");
        assert!((parsed.load - 0.42).abs() < 1e-6);
        assert!(parsed.available);
        assert_eq!(parsed.in_flight, 7);
    }
}
