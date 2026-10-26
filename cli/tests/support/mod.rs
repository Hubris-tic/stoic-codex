// Helpers are shared by several test binaries; not every binary uses every one.
#![allow(dead_code)]

//! A one-shot HTTP server on loopback, for tests that need a real socket.
//!
//! The point is fidelity: `zenithal run` is tested through its actual transport, so the
//! request line, headers, framing and streaming are all exercised — while still touching
//! nothing outside 127.0.0.1 and needing no key, no vendor, and no network.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

pub struct Stub {
    pub port: u16,
    requests: Arc<Mutex<Vec<String>>>,
    handle: Option<JoinHandle<()>>,
}

impl Stub {
    /// Start a server that answers the first request with `response` and then shuts down.
    pub fn start(response: impl Into<Vec<u8>>) -> Stub {
        Stub::start_n(1, response)
    }

    /// Answers up to `n` requests with the same response.
    pub fn start_n(n: usize, response: impl Into<Vec<u8>>) -> Stub {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().unwrap().port();
        let response = response.into();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let sink = requests.clone();
        let handle = std::thread::spawn(move || {
            for _ in 0..n {
                let Ok((stream, _)) = listener.accept() else {
                    return;
                };
                let request = read_request(&stream);
                sink.lock().unwrap().push(request);
                let mut stream = stream;
                let _ = stream.write_all(&response);
                let _ = stream.flush();
                let _ = stream.shutdown(std::net::Shutdown::Write);
                // Drain whatever the client still sends so it never sees a reset.
                let mut junk = [0u8; 256];
                let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                let _ = stream.read(&mut junk);
            }
        });
        Stub {
            port,
            requests,
            handle: Some(handle),
        }
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// The raw requests (request line, headers, blank line, body).
    ///
    /// Empty entries are filtered: `join` and `Drop` both open a throwaway connection to
    /// wake a blocked `accept`, and that connection is not a request.
    pub fn requests(&self) -> Vec<String> {
        let mut all = self.requests.lock().unwrap().clone();
        all.retain(|r| !r.is_empty());
        all
    }

    /// Wait for the server to finish, then hand back what it saw.
    pub fn join(mut self) -> Vec<String> {
        if let Some(h) = self.handle.take() {
            // `accept` is blocking by design; a connection to a closed-by-us listener is
            // the portable way to wake it.
            let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
            let _ = h.join();
        }
        self.requests()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            // Do not block the test on a connection the client never made: the thread
            // will exit as soon as the listener is dropped below.
            let _ = std::net::TcpStream::connect(("127.0.0.1", self.port));
            let _ = h.join();
        }
    }
}

/// Read one HTTP request: headers, then exactly `content-length` body bytes.
fn read_request(stream: &TcpStream) -> String {
    let mut stream = stream.try_clone().expect("clone stream");
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        if let Some(end) = find(&buf, b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..end]).to_string();
            let want = head
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    if k.eq_ignore_ascii_case("content-length") {
                        v.trim().parse::<usize>().ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(0);
            let body_start = end + 4;
            while buf.len() < body_start + want {
                let n = stream.read(&mut chunk).unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
            }
            return String::from_utf8_lossy(&buf).to_string();
        }
        let n = stream.read(&mut chunk).unwrap_or(0);
        if n == 0 {
            return String::from_utf8_lossy(&buf).to_string();
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// An SSE response as an OpenAI-compatible server would send it.
pub fn openai_sse(text_chunks: &[&str]) -> Vec<u8> {
    let mut body = String::new();
    for (i, piece) in text_chunks.iter().enumerate() {
        let delta = serde_json::json!({
            "id": "chatcmpl-test",
            "object": "chat.completion.chunk",
            "model": "test-model",
            "choices": [{ "index": 0, "delta": { "content": piece }, "finish_reason": serde_json::Value::Null }],
        });
        body.push_str(&format!("data: {delta}\n\n"));
        let _ = i;
    }
    let usage = serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion.chunk",
        "model": "test-model",
        "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }],
        "usage": { "prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18 },
    });
    body.push_str(&format!("data: {usage}\n\n"));
    body.push_str("data: [DONE]\n\n");
    with_headers(&body, "text/event-stream")
}

/// A whole JSON completion, for servers that ignore `stream: true`.
pub fn openai_json(text: &str) -> Vec<u8> {
    let body = serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": { "role": "assistant", "content": text },
            "finish_reason": "stop"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 },
    });
    with_headers(&body.to_string(), "application/json")
}

pub fn error_body(status_line: &str, body: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 {status_line}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn with_headers(body: &str, content_type: &str) -> Vec<u8> {
    format!(
        "HTTP/1.1 200 OK\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// A port nobody is listening on: bind, read the port, drop the listener.
pub fn closed_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// A whole JSON completion that calls one tool — the shape a model uses to act.
pub fn openai_tool_call(name: &str, args: &serde_json::Value) -> Vec<u8> {
    let body = serde_json::json!({
        "id": "chatcmpl-test",
        "object": "chat.completion",
        "model": "test-model",
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": serde_json::Value::Null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": name, "arguments": args.to_string() }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 5, "completion_tokens": 3, "total_tokens": 8 },
    });
    with_headers(&body.to_string(), "application/json")
}

impl Stub {
    /// Answer requests from a script: the Nth request gets `responses[N]`.
    ///
    /// This is how a multi-agent turn is tested at the CLI level — one socket, a fixed
    /// sequence of model replies, and no dependence on any real model. Extra requests reuse
    /// the last reply, so an unexpected extra turn cannot hang the test.
    pub fn script(tag: &str, responses: Vec<Vec<u8>>) -> Stub {
        assert!(!responses.is_empty(), "{tag}: empty script");
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let sink = requests.clone();
        let handle = std::thread::spawn(move || {
            let last = responses.len() - 1;
            let mut served = 0usize;
            let started = std::time::Instant::now();
            loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let request = read_request(&stream);
                        sink.lock().unwrap().push(request);
                        let response = &responses[served.min(last)];
                        let mut stream = stream;
                        let _ = stream.write_all(response);
                        let _ = stream.flush();
                        let _ = stream.shutdown(std::net::Shutdown::Write);
                        let mut junk = [0u8; 256];
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(100)));
                        let _ = stream.read(&mut junk);
                        served += 1;
                        if served > last {
                            // Everything scripted has been consumed; stay alive briefly so
                            // a trailing connection cannot look like a reset.
                            std::thread::sleep(Duration::from_millis(50));
                            return;
                        }
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if started.elapsed() > Duration::from_secs(20) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => return,
                }
            }
        });
        Stub {
            port,
            requests,
            handle: Some(handle),
        }
    }
}
