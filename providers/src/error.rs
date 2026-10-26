use std::fmt;

/// Errors produced by the wire layer. Deliberately dependency-free (no thiserror)
/// so this crate compiles fast and can be dropped into `codex-rs` without adding
/// proc-macro weight to the workspace build graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The canonical request cannot be expressed in this provider's protocol.
    Unsupported {
        provider: &'static str,
        feature: String,
    },
    /// The provider returned something we could not map. `snippet` is truncated.
    Protocol {
        provider: &'static str,
        detail: String,
        snippet: String,
    },
    /// An error payload the provider itself reported (4xx/5xx body or stream error).
    Remote {
        provider: &'static str,
        status: u16,
        kind: String,
        message: String,
    },
    /// Transport/auth config problem detected before sending.
    Config { detail: String },
}

impl Error {
    pub(crate) fn protocol(
        provider: &'static str,
        detail: impl Into<String>,
        snippet: &str,
    ) -> Self {
        let s: String = snippet.chars().take(240).collect();
        Error::Protocol {
            provider,
            detail: detail.into(),
            snippet: s,
        }
    }
    pub(crate) fn unsupported(provider: &'static str, feature: impl Into<String>) -> Self {
        Error::Unsupported {
            provider,
            feature: feature.into(),
        }
    }

    /// True when retrying the identical request is plausible (rate limit, upstream 5xx).
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::Remote { status, kind, .. } => {
                *status == 429 || *status >= 500 || kind == "overloaded_error"
            }
            _ => false,
        }
    }

    /// True when the request should be re-routed to a different provider rather than retried.
    pub fn is_fatal_for_provider(&self) -> bool {
        match self {
            Error::Unsupported { .. } => true,
            Error::Remote { status, .. } => *status == 401 || *status == 403 || *status == 404,
            Error::Config { .. } => true,
            _ => false,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported { provider, feature } => {
                write!(f, "{provider}: unsupported feature '{feature}'")
            }
            Error::Protocol {
                provider,
                detail,
                snippet,
            } => {
                write!(f, "{provider}: protocol error: {detail} (near: {snippet})")
            }
            Error::Remote {
                provider,
                status,
                kind,
                message,
            } => {
                write!(f, "{provider}: HTTP {status} {kind}: {message}")
            }
            Error::Config { detail } => write!(f, "config: {detail}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;
