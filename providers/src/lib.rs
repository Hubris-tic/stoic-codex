//! # zenithal-providers
//!
//! Provider-agnostic wire layer for the fork's BYOK requirement.
//!
//! ## Shape
//!
//! ```text
//!   agent code ──► CanonicalRequest (IR) ──► WireAdapter ──► vendor JSON/SSE
//!   agent code ◄── CanonicalEvent         ◄── StreamDecoder ◄── vendor JSON/SSE
//! ```
//!
//! Every provider-specific oddity (Anthropic's system-only-tools placement,
//! Gemini's non-delta function calls, OpenAI's fragmented tool arguments) is
//! confined to one adapter file. Adding a provider never touches agent code.
//!
//! ## Why no async / no HTTP here
//!
//! Adapters must be testable against recorded cassettes in CI with no keys and no
//! network. I/O belongs to the existing `http-client` crate; this crate is pure
//! translation, which is what makes the conformance suite possible.

pub mod adapter;
pub mod adapters;
pub mod capabilities;
pub mod cost;
pub mod custom;
pub mod error;
pub mod ir;
pub mod models;
pub mod registry;
pub mod router;
pub mod sse;
pub mod tokens;

pub use adapter::{AuthSpec, StreamDecoder, WireAdapter, WireRequest};
pub use capabilities::{Capabilities, Feature};
pub use custom::{CustomProvider, CustomSpec};
pub use error::{Error, Result};
pub use ir::{
    CacheStrategy, CanonicalEvent, CanonicalRequest, CanonicalResponse, Content, Message, Role,
    StopReason, StreamAccumulator, ToolChoice, ToolSpec, Usage,
};
pub use registry::{ModelSpec, Registry};
pub use router::{Requirements, Router, Selection, TaskClass};
pub use sse::{SseDecoder, SseFrame};

/// Build the adapter for a provider id. Vendors that speak OpenAI's protocol are
/// served by the compat presets in [`adapters::compat`].
pub fn adapter_for(provider_id: &str) -> Result<Box<dyn WireAdapter>> {
    adapters::by_id(provider_id)
}
