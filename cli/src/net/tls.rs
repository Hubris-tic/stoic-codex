//! TLS for the real transport: rustls + the operating system trust store.
//!
//! Local-first does not mean unencrypted. A user pointing `stoic-codex` at their own machine
//! uses `http://127.0.0.1`, but the moment the endpoint is a hosted provider the
//! connection must still be authenticated — otherwise BYOK would mean "send my key in
//! plaintext". No certificate pinning, no custom root store, no disabling verification:
//! the same authorities your browser trusts, and nothing else.

use std::sync::{Arc, OnceLock};

use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::ServerName;

use crate::net::NetError;

/// Build (once) and reuse a client config. Loading the system bundle costs a few
/// milliseconds, which matters when every command is a fresh process.
pub fn client_config() -> Result<Arc<ClientConfig>, NetError> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    if let Some(cfg) = CONFIG.get() {
        return Ok(cfg.clone());
    }
    // With one crypto provider compiled in, this is a no-op that just makes the choice
    // explicit; if a future build enables a second provider this prevents a panic.
    let _ = rustls::crypto::ring::default_provider().install_default();

    let loaded = rustls_native_certs::load_native_certs();
    if loaded.certs.is_empty() {
        return Err(NetError::Tls {
            host: "(system trust store)".into(),
            detail: "no root certificates found; https is unavailable on this machine".into(),
        });
    }
    let mut roots = RootCertStore::empty();
    let mut added = 0usize;
    for cert in loaded.certs {
        if roots.add(cert).is_ok() {
            added += 1;
        }
    }
    if added == 0 {
        return Err(NetError::Tls {
            host: "(system trust store)".into(),
            detail: "none of the system root certificates could be loaded".into(),
        });
    }
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let config = Arc::new(config);
    // A racing thread may have set it first; either Arc is equivalent.
    let _ = CONFIG.set(config.clone());
    Ok(CONFIG.get().cloned().unwrap_or(config))
}

/// Turn a hostname into a `ServerName`, rejecting what rustls cannot verify.
pub fn server_name(host: &str) -> Result<ServerName<'static>, NetError> {
    ServerName::try_from(host.to_string()).map_err(|e| NetError::Tls {
        host: host.to_string(),
        detail: format!("`{host}` is not a valid certificate name: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hostname_becomes_a_server_name() {
        assert!(server_name("api.anthropic.com").is_ok());
        assert!(server_name("127.0.0.1").is_ok());
    }

    #[test]
    fn the_client_config_loads_from_the_system_store() {
        // On a machine without any trust anchors this is a clean error, not a panic —
        // which is the property that matters for a CLI a user may run in a container.
        match client_config() {
            Ok(cfg) => assert!(!cfg.alpn_protocols.iter().any(|p| p == b"h2")),
            Err(e) => assert!(e.to_string().contains("trust") || e.to_string().contains("root")),
        }
    }
}
