//! Live TLS check — the one test in this tree that touches the public internet.
//!
//! Marked `#[ignore]` so the normal suite stays hermetic and offline-safe — `cargo test`
//! reports them as ignored rather than counting them as passes. Their job is narrow: prove
//! that a real certificate chain from the system trust store is accepted and that the HTTP
//! framing survives TLS, because "rustls compiles" is not the same claim as "a hosted
//! provider can be reached".
//!
//! ```bash
//! cargo test --test net_live -- --ignored
//! ```

use std::time::Duration;

use zenithal_cli::net::{HttpRequest, LiveTransport, Transport, Url};

fn post(url: &str) -> Result<(u16, Vec<(String, String)>), zenithal_cli::net::NetError> {
    let url = Url::parse(url)?;
    let req = HttpRequest {
        method: "GET".to_string(),
        url,
        headers: vec![("accept".into(), "*/*".into())],
        body: Vec::new(),
    };
    let transport = LiveTransport::new(Duration::from_secs(20));
    let mut body_len = 0usize;
    let response = transport.send(&req, &mut |chunk| body_len += chunk.len())?;
    assert!(body_len > 0, "a successful GET should have a body");
    Ok((response.status, response.headers))
}

#[test]
#[ignore = "needs network: cargo test --test net_live -- --ignored"]
fn a_real_certificate_chain_is_accepted() {
    let (status, headers) = post("https://example.com/").expect("TLS + HTTP against example.com");
    assert!(
        status == 200 || (300..400).contains(&status),
        "unexpected status {status}"
    );
    assert!(
        headers.iter().any(|(k, _)| k == "content-type"),
        "response headers were not parsed: {headers:?}"
    );
}

#[test]
#[ignore = "needs network: cargo test --test net_live -- --ignored"]
fn a_name_that_does_not_match_the_certificate_is_refused() {
    // This host serves a certificate for a different name on purpose.
    match post("https://wrong.host.badssl.com/") {
        Ok((status, _)) => {
            panic!("a mismatched certificate must not be accepted (status {status})")
        }
        Err(e) => {
            let text = e.to_string().to_lowercase();
            assert!(
                text.contains("tls") || text.contains("certificate") || text.contains("connect"),
                "unexpected failure: {e}"
            );
        }
    }
}
