//! Incremental Server-Sent Events decoder.
//!
//! Deliberately hand-rolled and dependency-free: every provider streams SSE, the
//! framing rules are tiny, and the failure modes (chunk boundaries splitting a
//! field, CRLF vs LF, multi-line `data:`, `[DONE]` sentinels) are exactly where
//! agent streaming bugs live. One implementation, one test suite.

/// One dispatched SSE event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseFrame {
    /// Value of the `event:` field, if the stream set one.
    pub event: Option<String>,
    /// All `data:` lines joined with newlines; may legitimately be `[DONE]`.
    pub data: String,
    pub id: Option<String>,
}

impl SseFrame {
    pub fn is_done_sentinel(&self) -> bool {
        self.data.trim() == "[DONE]"
    }
}

#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: String,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed an arbitrary chunk (which may split a line, a field, or a UTF-8 code
    /// point boundary — callers must pass valid UTF-8, i.e. decode bytes first).
    /// Returns every frame completed by this chunk, in order.
    pub fn push(&mut self, chunk: &str) -> Vec<SseFrame> {
        self.buf.push_str(chunk);
        // Normalize CRLF without ever emitting a half-handled trailing '\r':
        // if the buffer ends with '\r' we leave it for the next push.
        if self.buf.contains('\r') {
            self.buf = self.buf.replace("\r\n", "\n");
        }
        let mut frames = Vec::new();
        loop {
            // A frame is terminated by a blank line.
            let Some(idx) = find_frame_end(&self.buf) else {
                break;
            };
            let raw: String = self.buf.drain(..idx).collect();
            // Consume the blank-line separator itself.
            let skip = if self.buf.starts_with("\r\n\r\n") {
                4
            } else {
                2
            };
            if self.buf.len() >= skip && self.buf[..skip].chars().all(|c| c == '\n' || c == '\r') {
                self.buf.drain(..skip);
            }
            if let Some(f) = parse_frame(&raw) {
                frames.push(f);
            }
        }
        frames
    }

    /// Flush a trailing frame that was not followed by a blank line.
    /// Call once when the HTTP body ends.
    pub fn finish(&mut self) -> Option<SseFrame> {
        let rest = std::mem::take(&mut self.buf);
        if rest.trim().is_empty() {
            return None;
        }
        parse_frame(&rest)
    }

    /// Bytes currently buffered but not yet dispatched (for diagnostics).
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }
}

fn find_frame_end(s: &str) -> Option<usize> {
    if s.starts_with('\n') {
        // Empty frame at the very start; still a terminator.
        return Some(0);
    }
    s.find("\n\n").or_else(|| s.find("\r\n\r\n"))
}

fn parse_frame(raw: &str) -> Option<SseFrame> {
    let mut frame = SseFrame::default();
    let mut data_lines: Vec<&str> = Vec::new();
    let mut saw_field = false;

    for line in raw.lines() {
        if line.is_empty() {
            continue;
        }
        if let Some(comment) = line.strip_prefix(':') {
            let _ = comment; // heartbeat / keep-alive
            continue;
        }
        let (field, value) = match line.split_once(':') {
            Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
            None => (line, ""),
        };
        saw_field = true;
        match field {
            "data" => data_lines.push(value),
            "event" => frame.event = Some(value.to_string()),
            "id" => frame.id = Some(value.to_string()),
            "retry" => {}
            _ => {} // unknown fields are ignored per spec
        }
    }

    if !saw_field {
        return None;
    }
    frame.data = data_lines.join("\n");
    Some(frame)
}
