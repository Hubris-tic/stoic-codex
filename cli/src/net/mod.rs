//! A small blocking HTTP/1.1 client — the only I/O in this tree.
//!
//! Why hand-rolled: the crate graph must stay flat and start fast, and the provider
//! layer already models requests as data (`WireRequest`). All that is missing is a socket
//! and a TLS handshake, which is a few hundred lines rather than a dependency tree of
//! async runtimes and connection pools.
//!
//! Scope, stated honestly:
//! * `http` and `https` (rustls, system trust store);
//! * `Content-Length`, chunked, and read-to-EOF bodies;
//! * one request per connection (`Connection: close`) — no pooling, no keep-alive;
//! * the body is handed to the caller **as it arrives**, so a streaming completion can be
//!   printed while it is being generated instead of after.
//!
//! Out of scope: HTTP/2, redirects (reported, not followed), proxies, cookies, gzip.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

pub mod tls;

/// A fully resolved request: URL parsed, headers assembled, body serialized.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Status and headers. The body has already been delivered to the sink.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Vec<(String, String)>,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// True when the body is an SSE stream, per the response's own content type.
    pub fn is_event_stream(&self) -> bool {
        self.header("content-type")
            .map(|v| v.to_ascii_lowercase().contains("text/event-stream"))
            .unwrap_or(false)
    }
}

/// The seam that makes the turn testable: a stub can answer from memory, and a test can
/// point the real client at a loopback server.
pub trait Transport {
    /// Perform one request. Body bytes are passed to `sink` as they arrive.
    fn send(&self, req: &HttpRequest, sink: &mut dyn FnMut(&str)) -> Result<Response, NetError>;

    /// Perform one request, telling the sink the status **before** the first body byte.
    ///
    /// This exists for exactly one reason: a retry layer has to know whether the bytes it is
    /// about to hand the user are an error document or the real stream, and by the time
    /// [`send`](Transport::send) returns it is too late to withhold them. HTTP puts the status
    /// ahead of the body, so the information is there — this just carries it.
    ///
    /// The default reports `0`, meaning "unknown", which callers must treat as success. Stubs
    /// that answer from memory can therefore ignore it; anything reading a socket should not.
    fn send_statused(
        &self,
        req: &HttpRequest,
        sink: &mut dyn FnMut(u16, &str),
    ) -> Result<Response, NetError> {
        self.send(req, &mut |chunk| sink(0, chunk))
    }

    /// Perform one request, respecting an absolute deadline when the transport supports it.
    /// In-memory transports may ignore the deadline; the live socket transport enforces it.
    fn send_with_deadline(
        &self,
        req: &HttpRequest,
        sink: &mut dyn FnMut(&str),
        _deadline: Option<Instant>,
    ) -> Result<Response, NetError> {
        self.send(req, sink)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    BadUrl(String),
    Unsupported(String),
    Connect { address: String, detail: String },
    Tls { host: String, detail: String },
    Timeout { address: String, after: Duration },
    Protocol(String),
}

impl std::fmt::Display for NetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NetError::BadUrl(detail) => write!(f, "invalid URL: {detail}"),
            NetError::Unsupported(what) => write!(f, "{what}"),
            NetError::Connect { address, detail } => {
                write!(f, "cannot connect to {address}: {detail}")
            }
            NetError::Tls { host, detail } => {
                write!(f, "TLS handshake with {host} failed: {detail}")
            }
            NetError::Timeout { address, after } => {
                write!(
                    f,
                    "timed out after {}s talking to {address}",
                    after.as_secs()
                )
            }
            NetError::Protocol(detail) => write!(f, "malformed HTTP response: {detail}"),
        }
    }
}

impl std::error::Error for NetError {}

/// A parsed `http(s)://host[:port]/path?query`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    pub secure: bool,
    pub host: String,
    pub port: u16,
    /// Path plus query, always starting with `/`.
    pub path: String,
}

impl Url {
    pub fn parse(input: &str) -> Result<Url, NetError> {
        let trimmed = input.trim();
        let (scheme, rest) = trimmed.split_once("://").ok_or_else(|| {
            NetError::BadUrl(format!("`{trimmed}` has no scheme (try https://…)"))
        })?;
        let secure = match scheme.to_ascii_lowercase().as_str() {
            "http" => false,
            "https" => true,
            other => {
                return Err(NetError::Unsupported(format!(
                    "`{other}` is not supported; use http or https"
                )))
            }
        };
        if rest.is_empty() {
            return Err(NetError::BadUrl(format!("`{trimmed}` has no host")));
        }

        // Split authority from path. The cut is the first of '/', '?' or '#', so an IPv6
        // literal keeps its brackets and the colons inside them cannot be mistaken for a
        // port separator; '?' alone (`http://host?x=1`) is legal and has no path.
        let cut = rest.find(['/', '?', '#']);
        let (authority, path) = match cut {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        // Fragments are never sent to a server.
        let path = path.split('#').next().unwrap_or("");
        // Drop any userinfo: credentials belong in a header, never in a URL.
        let authority = authority.rsplit('@').next().unwrap_or(authority);
        if authority.is_empty() {
            return Err(NetError::BadUrl(format!("`{trimmed}` has no host")));
        }

        let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
            let end = inner.find(']').ok_or_else(|| {
                NetError::BadUrl(format!("`{authority}` is not a valid IPv6 host"))
            })?;
            let host = &inner[..end];
            let after = &inner[end + 1..];
            let port = match after.strip_prefix(':') {
                Some(p) => Some(p),
                None if after.is_empty() => None,
                None => {
                    return Err(NetError::BadUrl(format!(
                        "`{authority}` has a trailing `{after}`"
                    )))
                }
            };
            (host.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) if p.is_empty() => (h.to_string(), None),
                Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => (h.to_string(), Some(p)),
                Some((_, p)) => {
                    return Err(NetError::BadUrl(format!("`{p}` is not a port number")))
                }
                None => (authority.to_string(), None),
            }
        };
        if host.is_empty() {
            return Err(NetError::BadUrl(format!("`{trimmed}` has no host")));
        }
        let port = match port {
            Some(p) => p
                .parse::<u16>()
                .map_err(|_| NetError::BadUrl(format!("`{p}` is not a port")))?,
            None if secure => 443,
            None => 80,
        };
        let path = if path.is_empty() || path.starts_with('?') {
            format!("/{}", path)
        } else {
            path.to_string()
        };
        Ok(Url {
            secure,
            host,
            port,
            path,
        })
    }

    /// `host:port` — what error messages should name.
    pub fn authority(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    /// Redacted form for logs: no query string, because some servers take keys there.
    pub fn display_redacted(&self) -> String {
        let path = self.path.split('?').next().unwrap_or("/");
        let scheme = if self.secure { "https" } else { "http" };
        format!("{scheme}://{}{path}", self.authority())
    }
}

impl std::fmt::Display for Url {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_redacted())
    }
}

/// The real transport: TCP, optionally TLS, one request per connection.
pub struct LiveTransport {
    timeout: Duration,
}

impl LiveTransport {
    pub fn new(timeout: Duration) -> Self {
        LiveTransport { timeout }
    }

    pub fn with_default_timeout() -> Self {
        LiveTransport::new(Duration::from_secs(120))
    }

    fn timeout_for(&self, address: &str, deadline: Option<Instant>) -> Result<Duration, NetError> {
        let timeout = match deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(NetError::Timeout {
                        address: address.to_string(),
                        after: Duration::ZERO,
                    });
                }
                self.timeout.min(remaining)
            }
            None => self.timeout,
        };
        Ok(timeout)
    }

    fn stream(&self, url: &Url, deadline: Option<Instant>) -> Result<Stream, NetError> {
        let address = url.authority();
        self.timeout_for(&address, deadline)?;
        // System DNS resolution is synchronous and cannot be interrupted by std. Recheck the
        // deadline immediately afterward so a slow resolver cannot start a late socket attempt.
        let addrs: Vec<_> = (url.host.as_str(), url.port)
            .to_socket_addrs()
            .map_err(|e| NetError::Connect {
                address: address.clone(),
                detail: e.to_string(),
            })?
            .collect();
        if addrs.is_empty() {
            return Err(NetError::Connect {
                address: address.clone(),
                detail: "host did not resolve to any address".into(),
            });
        }
        let connect_timeout = self.timeout_for(&address, deadline)?;
        let tcp = TcpStream::connect_timeout(&addrs[0], connect_timeout).map_err(|e| {
            if e.kind() == std::io::ErrorKind::TimedOut {
                NetError::Timeout {
                    address: address.clone(),
                    after: connect_timeout,
                }
            } else {
                NetError::Connect {
                    address: address.clone(),
                    detail: e.to_string(),
                }
            }
        })?;
        let socket_timeout = self.timeout_for(&address, deadline)?;
        tcp.set_read_timeout(Some(socket_timeout)).ok();
        tcp.set_write_timeout(Some(socket_timeout)).ok();
        tcp.set_nodelay(true).ok();

        if !url.secure {
            return Ok(Stream::Plain(tcp));
        }
        let config = tls::client_config()?;
        let server_name = tls::server_name(&url.host)?;
        let conn =
            rustls::ClientConnection::new(config, server_name).map_err(|e| NetError::Tls {
                host: url.host.clone(),
                detail: e.to_string(),
            })?;
        Ok(Stream::Tls(Box::new(rustls::StreamOwned::new(conn, tcp))))
    }
}

impl Transport for LiveTransport {
    fn send(&self, req: &HttpRequest, sink: &mut dyn FnMut(&str)) -> Result<Response, NetError> {
        self.send_with_deadline(req, sink, None)
    }

    fn send_statused(
        &self,
        req: &HttpRequest,
        sink: &mut dyn FnMut(u16, &str),
    ) -> Result<Response, NetError> {
        let deadline = None;
        let mut stream =
            DeadlineStream::new(self.stream(&req.url, deadline)?, self.timeout, deadline);
        exchange_statused(&mut stream, req, sink, self.timeout)
    }

    fn send_with_deadline(
        &self,
        req: &HttpRequest,
        sink: &mut dyn FnMut(&str),
        deadline: Option<Instant>,
    ) -> Result<Response, NetError> {
        let request_timeout = self.timeout_for(&req.url.authority(), deadline)?;
        let inner = self.stream(&req.url, deadline)?;
        let mut stream = DeadlineStream::new(inner, self.timeout, deadline);
        exchange(&mut stream, req, sink, request_timeout)
    }
}

enum Stream {
    Plain(TcpStream),
    Tls(Box<rustls::StreamOwned<rustls::ClientConnection, TcpStream>>),
}

impl Stream {
    fn set_read_timeout(&self, timeout: Duration) -> std::io::Result<()> {
        match self {
            Stream::Plain(stream) => stream.set_read_timeout(Some(timeout)),
            Stream::Tls(stream) => stream.sock.set_read_timeout(Some(timeout)),
        }
    }

    fn set_write_timeout(&self, timeout: Duration) -> std::io::Result<()> {
        match self {
            Stream::Plain(stream) => stream.set_write_timeout(Some(timeout)),
            Stream::Tls(stream) => stream.sock.set_write_timeout(Some(timeout)),
        }
    }
}

/// Refresh socket timeouts before every outer I/O operation so a stream of individually-fast
/// reads cannot keep a mission request alive beyond its absolute retry deadline.
struct DeadlineStream {
    inner: Stream,
    timeout: Duration,
    deadline: Option<Instant>,
}

impl DeadlineStream {
    fn new(inner: Stream, timeout: Duration, deadline: Option<Instant>) -> Self {
        DeadlineStream {
            inner,
            timeout,
            deadline,
        }
    }

    fn timeout_for_io(&self) -> std::io::Result<Duration> {
        match self.deadline {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "mission retry deadline expired",
                    ));
                }
                Ok(self.timeout.min(remaining))
            }
            None => Ok(self.timeout),
        }
    }

    fn prepare_read(&self) -> std::io::Result<()> {
        self.inner.set_read_timeout(self.timeout_for_io()?)
    }

    fn prepare_write(&self) -> std::io::Result<()> {
        self.inner.set_write_timeout(self.timeout_for_io()?)
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.prepare_read()?;
        self.inner.read(buf)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.prepare_write()?;
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.prepare_write()?;
        self.inner.flush()
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.read(buf),
            Stream::Tls(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Stream::Plain(s) => s.write(buf),
            Stream::Tls(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Stream::Plain(s) => s.flush(),
            Stream::Tls(s) => s.flush(),
        }
    }
}

/// Write the request, then read the status line, headers and body, feeding the body to
/// `sink` as it arrives.
fn exchange(
    stream: &mut (impl Read + Write),
    req: &HttpRequest,
    sink: &mut dyn FnMut(&str),
    timeout: Duration,
) -> Result<Response, NetError> {
    exchange_statused(stream, req, &mut |_, chunk| sink(chunk), timeout)
}

fn exchange_statused(
    stream: &mut (impl Read + Write),
    req: &HttpRequest,
    sink: &mut dyn FnMut(u16, &str),
    timeout: Duration,
) -> Result<Response, NetError> {
    let io = |e: std::io::Error| -> NetError {
        if e.kind() == std::io::ErrorKind::TimedOut || e.kind() == std::io::ErrorKind::WouldBlock {
            NetError::Timeout {
                address: req.url.authority(),
                after: timeout,
            }
        } else {
            NetError::Protocol(e.to_string())
        }
    };

    let mut head = format!("{} {} HTTP/1.1\r\n", req.method, req.url.path);
    if req.header("host").is_none() {
        // A Host header is mandatory in HTTP/1.1 and virtual hosts break without it.
        head.push_str(&format!("host: {}\r\n", req.url.authority()));
    }
    for (k, v) in &req.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    if req.header("content-length").is_none() {
        head.push_str(&format!("content-length: {}\r\n", req.body.len()));
    }
    if req.header("connection").is_none() {
        head.push_str("connection: close\r\n");
    }
    head.push_str("\r\n");

    stream.write_all(head.as_bytes()).map_err(io)?;
    if !req.body.is_empty() {
        stream.write_all(&req.body).map_err(io)?;
    }
    stream.flush().map_err(io)?;

    let (status, headers, mut leftover) = read_head(stream).map_err(io)?;
    let response = Response { status, headers };
    read_body(stream, &response.headers, &mut leftover, &mut |chunk| {
        sink(status, chunk)
    })
    .map_err(io)?;
    Ok(response)
}

/// Read until the blank line. Returns the status code, headers, and any body bytes that
/// arrived in the same read as the headers.
fn read_head(stream: &mut impl Read) -> std::io::Result<(u16, Vec<(String, String)>, Vec<u8>)> {
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut chunk = [0u8; 1024];
    let header_end;
    loop {
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            header_end = i + 4;
            break;
        }
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before the response headers were complete",
            ));
        }
        buf.extend_from_slice(&chunk[..n]);
        if buf.len() > 64 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "response headers exceeded 64 KiB",
            ));
        }
    }

    let text = String::from_utf8_lossy(&buf[..header_end - 4]).to_string();
    let leftover = buf[header_end..].to_vec();
    let mut lines = text.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    // `HTTP/1.1 200 OK`
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad status line `{status_line}`"),
            )
        })?;
    let mut headers = Vec::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    Ok((status, headers, leftover))
}

fn read_body(
    stream: &mut impl Read,
    headers: &[(String, String)],
    leftover: &mut Vec<u8>,
    sink: &mut dyn FnMut(&str),
) -> std::io::Result<()> {
    let get = |name: &str| {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let chunked = get("transfer-encoding")
        .map(|v| v.to_ascii_lowercase().contains("chunked"))
        .unwrap_or(false);
    let mut utf8 = Utf8Sink::new(sink);

    if chunked {
        let mut buf = std::mem::take(leftover);
        loop {
            match decode_chunked(&mut buf, &mut utf8, stream)? {
                Decoded::NeedMore => {
                    let mut chunk = [0u8; 8192];
                    let n = stream.read(&mut chunk)?;
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                }
                Decoded::Done => break,
            }
        }
        utf8.finish();
        return Ok(());
    }

    if let Some(len) = get("content-length").and_then(|v| v.parse::<usize>().ok()) {
        let mut remaining = len;
        if leftover.len() >= remaining {
            utf8.push(&leftover[..remaining]);
            utf8.finish();
            return Ok(());
        }
        utf8.push(leftover);
        remaining -= leftover.len();
        let mut chunk = [0u8; 8192];
        while remaining > 0 {
            let want = remaining.min(chunk.len());
            let n = stream.read(&mut chunk[..want])?;
            if n == 0 {
                break;
            }
            utf8.push(&chunk[..n]);
            remaining -= n;
        }
        utf8.finish();
        return Ok(());
    }

    // No framing at all: the server closes the connection to end the body.
    utf8.push(leftover);
    let mut chunk = [0u8; 8192];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        utf8.push(&chunk[..n]);
    }
    utf8.finish();
    Ok(())
}

enum Decoded {
    NeedMore,
    Done,
}

/// Decode as many complete chunks as `buf` holds, draining what it consumed.
fn decode_chunked(
    buf: &mut Vec<u8>,
    sink: &mut Utf8Sink<'_>,
    _stream: &mut impl Read,
) -> std::io::Result<Decoded> {
    loop {
        let Some(line_end) = find(buf, b"\r\n") else {
            return Ok(Decoded::NeedMore);
        };
        let size_line = String::from_utf8_lossy(&buf[..line_end]).to_string();
        // `1a;ext=1` — extension after ';' is legal and ignored.
        let size_hex = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_hex, 16).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad chunk size `{size_hex}`"),
            )
        })?;
        let body_start = line_end + 2;
        if size == 0 {
            return Ok(Decoded::Done);
        }
        if buf.len() < body_start + size + 2 {
            return Ok(Decoded::NeedMore);
        }
        sink.push(&buf[body_start..body_start + size]);
        buf.drain(..body_start + size + 2);
    }
}

/// Decode UTF-8 incrementally: a multi-byte character can be split across reads, and
/// printing half of one would corrupt the output.
struct Utf8Sink<'a> {
    sink: &'a mut dyn FnMut(&str),
    pending: Vec<u8>,
}

impl<'a> Utf8Sink<'a> {
    fn new(sink: &'a mut dyn FnMut(&str)) -> Self {
        Utf8Sink {
            sink,
            pending: Vec::new(),
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if self.pending.is_empty() {
            self.emit(bytes);
        } else {
            let mut joined = std::mem::take(&mut self.pending);
            joined.extend_from_slice(bytes);
            self.emit(&joined);
        }
    }

    fn emit(&mut self, bytes: &[u8]) {
        match std::str::from_utf8(bytes) {
            Ok(text) => {
                if !text.is_empty() {
                    (self.sink)(text);
                }
            }
            Err(e) => {
                let valid = e.valid_up_to();
                if valid > 0 {
                    // Safe: `valid_up_to` is the length of the valid prefix.
                    let text = std::str::from_utf8(&bytes[..valid]).unwrap_or("");
                    if !text.is_empty() {
                        (self.sink)(text);
                    }
                }
                self.pending = bytes[valid..].to_vec();
            }
        }
    }

    fn finish(&mut self) {
        if !self.pending.is_empty() {
            let text = String::from_utf8_lossy(&std::mem::take(&mut self.pending)).to_string();
            (self.sink)(&text);
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mission_deadline_interrupts_an_in_flight_socket_read() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 1024];
            socket
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let _ = socket.read(&mut request).unwrap();
            std::thread::sleep(Duration::from_millis(300));
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        });

        let client = TcpStream::connect(address).unwrap();
        let mut stream = DeadlineStream::new(
            Stream::Plain(client),
            Duration::from_secs(2),
            Some(Instant::now() + Duration::from_millis(100)),
        );
        let request = HttpRequest {
            method: "GET".into(),
            url: Url::parse(&format!("http://{address}/slow")).unwrap(),
            headers: Vec::new(),
            body: Vec::new(),
        };
        let error = exchange(&mut stream, &request, &mut |_| {}, Duration::from_secs(2))
            .expect_err("the delayed response must be cut off by the absolute deadline");
        assert!(matches!(error, NetError::Timeout { .. }), "{error:?}");
        drop(stream);
        server.join().unwrap();
    }

    #[test]
    fn parses_scheme_host_port_and_path() {
        let u = Url::parse("https://api.example.com/v1/messages").unwrap();
        assert!(u.secure);
        assert_eq!(u.host, "api.example.com");
        assert_eq!(u.port, 443);
        assert_eq!(u.path, "/v1/messages");

        let u = Url::parse("http://127.0.0.1:11434/api/chat").unwrap();
        assert!(!u.secure);
        assert_eq!(u.port, 11434);
        assert_eq!(u.authority(), "127.0.0.1:11434");
    }

    #[test]
    fn defaults_the_path_to_slash() {
        assert_eq!(Url::parse("http://localhost").unwrap().path, "/");
        assert_eq!(
            Url::parse("http://localhost:8080?x=1").unwrap().path,
            "/?x=1"
        );
    }

    #[test]
    fn keeps_a_query_string() {
        let u = Url::parse("https://example.com/v1?key=abc&b=2").unwrap();
        assert_eq!(u.path, "/v1?key=abc&b=2");
        // The redacted form must not leak a key that ended up in the query.
        assert_eq!(u.display_redacted(), "https://example.com:443/v1");
    }

    #[test]
    fn supports_ipv6_literals() {
        let u = Url::parse("http://[::1]:8080/x").unwrap();
        assert_eq!(u.host, "::1");
        assert_eq!(u.port, 8080);
        assert_eq!(u.authority(), "[::1]:8080");
    }

    #[test]
    fn drops_userinfo_rather_than_sending_it() {
        let u = Url::parse("https://user:pass@example.com/v1").unwrap();
        assert_eq!(u.host, "example.com");
        assert_eq!(u.path, "/v1");
        assert_eq!(u.display_redacted(), "https://example.com:443/v1");
    }

    #[test]
    fn rejects_what_it_cannot_do() {
        assert!(matches!(
            Url::parse("example.com/v1"),
            Err(NetError::BadUrl(_))
        ));
        assert!(matches!(
            Url::parse("ftp://example.com"),
            Err(NetError::Unsupported(_))
        ));
        assert!(matches!(Url::parse("http://"), Err(NetError::BadUrl(_))));
        assert!(matches!(
            Url::parse("http://host:notaport"),
            Err(NetError::BadUrl(_))
        ));
    }

    #[test]
    fn reads_a_head_and_keeps_the_leftover_body() {
        let raw = b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\nx-y: 1\r\n\r\n{\"a\":1}";
        let mut cursor = std::io::Cursor::new(raw.to_vec());
        let (status, headers, leftover) = read_head(&mut cursor).unwrap();
        assert_eq!(status, 200);
        assert_eq!(headers.len(), 2);
        assert_eq!(header(&headers, "content-type"), Some("application/json"));
        assert_eq!(leftover, b"{\"a\":1}");
    }

    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    #[test]
    fn reads_a_content_length_body_including_split_utf8() {
        let body = "héllo wörld".as_bytes();
        let mut raw =
            format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len()).into_bytes();
        raw.extend_from_slice(body);
        let mut cursor = std::io::Cursor::new(raw);
        let (_, headers, mut leftover) = read_head(&mut cursor).unwrap();
        let mut got = String::new();
        read_body(&mut cursor, &headers, &mut leftover, &mut |s| {
            got.push_str(s)
        })
        .unwrap();
        assert_eq!(got, "héllo wörld");
    }

    #[test]
    fn decodes_chunked_bodies_with_extensions() {
        let raw = b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5;ext=1\r\nhello\r\n1\r\n \r\n5\r\nworld\r\n0\r\n\r\n";
        let mut cursor = std::io::Cursor::new(raw.to_vec());
        let (_, headers, mut leftover) = read_head(&mut cursor).unwrap();
        let mut got = String::new();
        read_body(&mut cursor, &headers, &mut leftover, &mut |s| {
            got.push_str(s)
        })
        .unwrap();
        assert_eq!(got, "hello world");
    }

    #[test]
    fn a_body_without_framing_is_read_to_eof() {
        let raw = b"HTTP/1.1 200 OK\r\n\r\nstreamed to the end";
        let mut cursor = std::io::Cursor::new(raw.to_vec());
        let (_, headers, mut leftover) = read_head(&mut cursor).unwrap();
        let mut got = String::new();
        read_body(&mut cursor, &headers, &mut leftover, &mut |s| {
            got.push_str(s)
        })
        .unwrap();
        assert_eq!(got, "streamed to the end");
    }

    #[test]
    fn a_truncated_head_is_an_error_not_a_silent_empty_response() {
        let mut cursor = std::io::Cursor::new(b"HTTP/1.1 200 OK\r\n".to_vec());
        assert!(read_head(&mut cursor).is_err());
    }

    #[test]
    fn error_messages_name_the_address() {
        let e = NetError::Connect {
            address: "127.0.0.1:11434".into(),
            detail: "refused".into(),
        };
        assert!(e.to_string().contains("127.0.0.1:11434"));
        assert!(e.to_string().contains("refused"));
    }
}
