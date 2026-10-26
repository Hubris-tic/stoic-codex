//! Capability model.
//!
//! The old assumption baked into most agents is "the model is OpenAI-shaped".
//! That assumption is what breaks when you add a second provider. Capabilities are
//! the *only* thing the harness is allowed to assume, and every adapter declares
//! them so the router can gate on reality instead of hope.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    ToolCalling,
    ParallelToolCalls,
    Streaming,
    /// `usage` present in the final stream chunk (OpenAI needs an opt-in flag).
    StreamingUsage,
    Vision,
    /// Provider caches stable prefixes automatically (OpenAI, Gemini implicit).
    ImplicitPromptCache,
    /// Provider needs explicit `cache_control` breakpoints (Anthropic).
    ExplicitCacheBreakpoints,
    Reasoning,
    JsonMode,
    /// System prompt is a first-class field rather than a message.
    SystemField,
    StopSequences,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoiceSupport {
    #[default]
    None,
    AutoOnly,
    Full,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub tools: bool,
    pub parallel_tools: bool,
    pub streaming: bool,
    pub stream_usage: bool,
    pub vision: bool,
    pub implicit_cache: bool,
    pub explicit_cache: bool,
    pub reasoning: bool,
    pub json_mode: bool,
    pub tool_choice: ToolChoiceSupport,
    pub stop_sequences: bool,
    pub max_context: u32,
    pub max_output: u32,
}

impl Default for Capabilities {
    fn default() -> Self {
        Capabilities {
            tools: false,
            parallel_tools: false,
            streaming: true,
            stream_usage: false,
            vision: false,
            implicit_cache: false,
            explicit_cache: false,
            reasoning: false,
            json_mode: false,
            tool_choice: ToolChoiceSupport::None,
            stop_sequences: true,
            max_context: 8_192,
            max_output: 4_096,
        }
    }
}

impl Capabilities {
    pub fn has(&self, f: Feature) -> bool {
        match f {
            Feature::ToolCalling => self.tools,
            Feature::ParallelToolCalls => self.parallel_tools,
            Feature::Streaming => self.streaming,
            Feature::StreamingUsage => self.stream_usage,
            Feature::Vision => self.vision,
            Feature::ImplicitPromptCache => self.implicit_cache,
            Feature::ExplicitCacheBreakpoints => self.explicit_cache,
            Feature::Reasoning => self.reasoning,
            Feature::JsonMode => self.json_mode,
            Feature::SystemField => true,
            Feature::StopSequences => self.stop_sequences,
        }
    }

    /// Capabilities that must degrade gracefully rather than fail the request.
    /// The harness calls this to decide what to strip before sending, so a missing
    /// feature costs fidelity, not a 400.
    pub fn missing(&self, needs: &Needs) -> Vec<String> {
        let mut out = Vec::new();
        if needs.tools && !self.tools {
            out.push("tool_calling".into());
        }
        if needs.parallel_tools && !self.parallel_tools {
            out.push("parallel_tool_calls".into());
        }
        if needs.vision && !self.vision {
            out.push("vision".into());
        }
        if needs.reasoning && !self.reasoning {
            out.push("reasoning".into());
        }
        if needs.json_mode && !self.json_mode {
            out.push("json_mode".into());
        }
        if needs.stream_usage && !self.stream_usage {
            out.push("streaming_usage".into());
        }
        out
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Needs {
    pub tools: bool,
    pub parallel_tools: bool,
    pub vision: bool,
    pub reasoning: bool,
    pub json_mode: bool,
    pub stream_usage: bool,
}
