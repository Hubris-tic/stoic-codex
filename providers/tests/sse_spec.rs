//! SSE framing edge cases. These are the failure modes that produce silent
//! truncation in production, so they get explicit coverage.

use zenithal_providers::SseDecoder;

fn frames(chunks: &[&str]) -> Vec<String> {
    let mut d = SseDecoder::new();
    let mut out = Vec::new();
    for c in chunks {
        for f in d.push(c) {
            out.push(f.data);
        }
    }
    if let Some(f) = d.finish() {
        out.push(f.data);
    }
    out
}

#[test]
fn lf_and_crlf_both_terminate() {
    assert_eq!(frames(&["data: a\n\n"]), vec!["a"]);
    assert_eq!(frames(&["data: b\r\n\r\n"]), vec!["b"]);
}

#[test]
fn crlf_split_across_chunks() {
    // The '\r' arrives alone; a naive impl emits a frame here and desyncs.
    assert_eq!(
        frames(&["data: x\r", "\ndata: y\r\n", "\r\n"]),
        vec!["x\ny"]
    );
}

#[test]
fn multiline_data_is_joined_with_newline() {
    assert_eq!(
        frames(&["data: line1\ndata: line2\n\n"]),
        vec!["line1\nline2"]
    );
}

#[test]
fn field_split_mid_name() {
    assert_eq!(frames(&["da", "ta: hello\n", "\n"]), vec!["hello"]);
}

#[test]
fn comments_and_blank_keepalives_are_ignored() {
    assert_eq!(frames(&[": heartbeat\n\n", "data: real\n\n"]), vec!["real"]);
}

#[test]
fn done_sentinel_is_recognized() {
    let mut d = SseDecoder::new();
    let f = d.push("data: [DONE]\n\n");
    assert_eq!(f.len(), 1);
    assert!(f[0].is_done_sentinel());
}

#[test]
fn value_leading_space_stripped_once() {
    // Per spec exactly one space after ':' is removed, not all whitespace.
    assert_eq!(frames(&["data:  two-spaces\n\n"]), vec![" two-spaces"]);
}

#[test]
fn no_colon_line_is_data_less_but_not_a_frame_loss() {
    // A bare "data" line is a valid empty field per the spec.
    let mut d = SseDecoder::new();
    let f = d.push("data\n\n");
    assert_eq!(f.len(), 1);
    assert_eq!(f[0].data, "");
}

#[test]
fn trailing_frame_without_blank_line_is_flushed() {
    let mut d = SseDecoder::new();
    assert!(d.push("data: tail").is_empty());
    let f = d.finish().expect("trailing frame flushed");
    assert_eq!(f.data, "tail");
}

#[test]
fn large_body_parses_without_quadratic_blowup() {
    // 10k frames, fed in 64-byte slices: guards against per-push full rescans.
    let mut body = String::new();
    for i in 0..10_000 {
        body.push_str(&format!("data: {{\"i\":{i}}}\n\n"));
    }
    let mut d = SseDecoder::new();
    let mut n = 0;
    for chunk in body.as_bytes().chunks(64) {
        n += d.push(std::str::from_utf8(chunk).unwrap()).len();
    }
    assert_eq!(n, 10_000);
}
