//! The agent loop: model → tool calls → tool results → model, until it stops.
//!
//! This is the whole of "tool calling" that is not vendor-specific. The vendor adapters
//! translate `CanonicalResponse` to and from their wire format (including the parts that
//! differ wildly: Anthropic's `tool_use` blocks, OpenAI's fragmented `tool_calls` deltas,
//! Gemini's non-delta function calls). This file only sees normalised events, so the loop
//! is the same code for every provider.
//!
//! Three invariants worth stating, because breaking any of them produces a confusing loop
//! rather than a crisp failure:
//!
//! 1. **A tool result always follows a tool call.** Every `ToolUse` gets exactly one
//!    `ToolResult`, in the same order, even when the tool blew up — providers reject a
//!    transcript where a call went unanswered.
//! 2. **A failed tool is not a failed turn.** The error text goes back to the model, which
//!    usually adapts. The run ends only on a transport/model error or a budget limit.
//! 3. **The loop is bounded.** `max_steps` and `max_tool_calls` stop a model that has
//!    decided to read the same file forever, and the outcome says which limit fired.

use zenithal_providers::{
    CanonicalRequest, CanonicalResponse, Content, Message, Role, StopReason, ToolChoice, Usage,
};

use crate::tool::{ToolOutcome, ToolRegistry};

/// What the loop needs from a provider. Deliberately one method: anything richer (streaming
/// deltas, retries, rate limits) is a wrapper around this, and keeping it single makes the
/// whole loop testable with a scripted model and no network.
pub trait Model: Send + Sync {
    fn complete(&self, request: &CanonicalRequest) -> Result<CanonicalResponse, ModelError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// The request never got an answer: connection, TLS, timeout.
    Transport(String),
    /// An answer arrived but could not be understood.
    Decode(String),
    /// The provider answered with an error status (auth, rate limit, quota).
    Refused { status: u16, detail: String },
}

impl std::fmt::Display for ModelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ModelError::Transport(d) => write!(f, "model request failed: {d}"),
            ModelError::Decode(d) => write!(f, "cannot decode the model response: {d}"),
            ModelError::Refused { status, detail } => {
                write!(f, "provider refused ({status}): {detail}")
            }
        }
    }
}

impl std::error::Error for ModelError {}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentConfig {
    /// Wire model name.
    pub model: String,
    pub system: Option<String>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Model turns before the loop stops.
    pub max_steps: usize,
    /// Tool invocations across the whole run.
    pub max_tool_calls: usize,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            model: String::new(),
            system: None,
            max_tokens: Some(4096),
            temperature: None,
            max_steps: 12,
            max_tool_calls: 48,
        }
    }
}

impl AgentConfig {
    pub fn new(model: impl Into<String>) -> Self {
        AgentConfig {
            model: model.into(),
            ..AgentConfig::default()
        }
    }
}

/// Progress, for a UI or a log. Owned values only, so a caller can forward them to another
/// thread without copying the transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Step {
        index: usize,
    },
    Text {
        text: String,
    },
    ToolCall {
        name: String,
        args: String,
    },
    ToolResult {
        name: String,
        ok: bool,
        summary: String,
        ms: u64,
    },
    Finished {
        reason: StopCause,
        text_chars: usize,
        tool_calls: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    /// The model produced an answer with no further tool calls.
    Answer,
    StepLimit,
    ToolLimit,
    /// The model signalled a stop (max tokens, content filter) with no answer text.
    ModelStopped,
}

impl StopCause {
    pub fn label(self) -> &'static str {
        match self {
            StopCause::Answer => "answered",
            StopCause::StepLimit => "stopped at the step limit",
            StopCause::ToolLimit => "stopped at the tool-call limit",
            StopCause::ModelStopped => "the model stopped",
        }
    }

    pub fn is_complete(self) -> bool {
        matches!(self, StopCause::Answer)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentOutcome {
    pub text: String,
    pub steps: usize,
    pub tool_calls: usize,
    pub usage: Usage,
    pub stop: StopCause,
    /// Every assistant/tool message, so a caller can persist or resume the transcript.
    pub transcript: Vec<Message>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    Model(ModelError),
    /// The registry was empty while the model asked for tools: a configuration mistake
    /// worth naming rather than letting the model flounder.
    NoTools {
        requested: String,
    },
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::Model(e) => write!(f, "{e}"),
            AgentError::NoTools { requested } => write!(
                f,
                "the model called `{requested}` but this agent has no tools registered"
            ),
        }
    }
}

impl std::error::Error for AgentError {}

/// Run one agent to completion.
pub fn run_agent(
    model: &dyn Model,
    tools: &ToolRegistry,
    task: &str,
    cfg: &AgentConfig,
    on_event: &mut dyn FnMut(Event),
) -> Result<AgentOutcome, AgentError> {
    let mut messages: Vec<Message> = vec![Message::user(task)];
    let mut usage = Usage::default();
    let mut tool_calls = 0usize;
    let mut answer = String::new();
    let specs = tools.specs();

    for step in 0..cfg.max_steps {
        on_event(Event::Step { index: step });

        let mut request = CanonicalRequest {
            model: cfg.model.clone(),
            system: cfg.system.clone(),
            messages: messages.clone(),
            tools: specs.clone(),
            tool_choice: if specs.is_empty() {
                None
            } else {
                Some(ToolChoice::Auto)
            },
            max_tokens: cfg.max_tokens,
            temperature: cfg.temperature,
            top_p: None,
            stop: Vec::new(),
            json_mode: false,
            cache: zenithal_providers::CacheStrategy::Off,
            reasoning: None,
            stream: Some(false),
            extra: Default::default(),
        };
        request.tools = specs.clone();

        let response = model.complete(&request).map_err(AgentError::Model)?;
        usage.merge_max(&response.usage);
        for content in &response.content {
            if let Content::Text { text } = content {
                answer.push_str(text);
                on_event(Event::Text { text: text.clone() });
            }
        }

        let calls: Vec<(String, String, serde_json::Value)> = response
            .content
            .iter()
            .filter_map(|c| match c {
                Content::ToolUse {
                    id, name, input, ..
                } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            })
            .collect();

        if calls.is_empty() {
            let cause = if response.stop_reason == StopReason::MaxTokens {
                StopCause::ModelStopped
            } else {
                StopCause::Answer
            };
            on_event(Event::Finished {
                reason: cause,
                text_chars: answer.chars().count(),
                tool_calls,
            });
            return Ok(AgentOutcome {
                text: answer,
                steps: step + 1,
                tool_calls,
                usage,
                stop: cause,
                transcript: messages,
            });
        }

        if tools.is_empty() {
            return Err(AgentError::NoTools {
                requested: calls[0].1.clone(),
            });
        }

        // The assistant turn goes in verbatim: tool_use blocks must round-trip or the
        // provider cannot match the results that follow.
        messages.push(Message {
            role: Role::Assistant,
            content: response.content.clone(),
        });

        let mut results: Vec<Content> = Vec::with_capacity(calls.len());
        for (id, name, input) in calls {
            if tool_calls >= cfg.max_tool_calls {
                // Answer the outstanding call anyway: an unanswered tool_use poisons the
                // transcript for any follow-up turn.
                results.push(Content::ToolResult {
                    tool_use_id: id,
                    content: vec![Content::text("not run: tool-call budget exhausted")],
                    is_error: true,
                });
                continue;
            }
            tool_calls += 1;
            let args = summarise_args(&input);
            on_event(Event::ToolCall {
                name: name.clone(),
                args,
            });
            let started = std::time::Instant::now();
            let outcome: ToolOutcome = tools.invoke(&name, &input);
            let ms = started.elapsed().as_millis() as u64;
            on_event(Event::ToolResult {
                name: name.clone(),
                ok: !outcome.is_error,
                summary: outcome.summary(120),
                ms,
            });
            results.push(Content::ToolResult {
                tool_use_id: id,
                content: vec![Content::text(outcome.content)],
                is_error: outcome.is_error,
            });
        }
        messages.push(Message {
            role: Role::Tool,
            content: results,
        });

        if tool_calls >= cfg.max_tool_calls {
            let cause = StopCause::ToolLimit;
            on_event(Event::Finished {
                reason: cause,
                text_chars: answer.chars().count(),
                tool_calls,
            });
            return Ok(AgentOutcome {
                text: answer,
                steps: step + 1,
                tool_calls,
                usage,
                stop: cause,
                transcript: messages,
            });
        }
    }

    on_event(Event::Finished {
        reason: StopCause::StepLimit,
        text_chars: answer.chars().count(),
        tool_calls,
    });
    Ok(AgentOutcome {
        text: answer,
        steps: cfg.max_steps,
        tool_calls,
        usage,
        stop: StopCause::StepLimit,
        transcript: messages,
    })
}

/// One line, bounded: this ends up in a terminal or a log, not in a model's context.
fn summarise_args(input: &serde_json::Value) -> String {
    let text = serde_json::to_string(input).unwrap_or_default();
    let mut s: String = text.chars().take(100).collect();
    if text.chars().count() > 100 {
        s.push('…');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    use zenithal_providers::{CanonicalResponse, Content};

    /// A model that answers from a script. Each call pops the next response; running out
    /// is a bug in the test, not a behaviour to paper over.
    struct Scripted {
        script: Mutex<Vec<CanonicalResponse>>,
        seen: Mutex<Vec<CanonicalRequest>>,
    }

    impl Scripted {
        fn new(script: Vec<CanonicalResponse>) -> Arc<Scripted> {
            Arc::new(Scripted {
                script: Mutex::new(script),
                seen: Mutex::new(Vec::new()),
            })
        }
        fn calls(&self) -> usize {
            self.seen.lock().unwrap().len()
        }
        fn requests(&self) -> Vec<CanonicalRequest> {
            self.seen.lock().unwrap().clone()
        }
    }

    impl Model for Arc<Scripted> {
        fn complete(&self, request: &CanonicalRequest) -> Result<CanonicalResponse, ModelError> {
            (**self).complete(request)
        }
    }

    impl Scripted {
        fn complete(&self, request: &CanonicalRequest) -> Result<CanonicalResponse, ModelError> {
            self.seen.lock().unwrap().push(request.clone());
            let mut script = self.script.lock().unwrap();
            if script.is_empty() {
                return Err(ModelError::Decode("script exhausted".into()));
            }
            Ok(script.remove(0))
        }
    }

    fn text(s: &str) -> CanonicalResponse {
        CanonicalResponse {
            model: "test".into(),
            content: vec![Content::text(s)],
            stop_reason: StopReason::EndTurn,
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Usage::default()
            },
        }
    }

    fn tool_call(id: &str, name: &str, input: serde_json::Value) -> CanonicalResponse {
        CanonicalResponse {
            model: "test".into(),
            content: vec![Content::tool_use(id, name, input)],
            stop_reason: StopReason::ToolUse,
            usage: Usage {
                input_tokens: 20,
                output_tokens: 8,
                ..Usage::default()
            },
        }
    }

    fn registry() -> ToolRegistry {
        let dir = std::env::temp_dir().join(format!(
            "zenithal-loop-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        ToolRegistry::editing(Arc::new(crate::sandbox::Sandbox::new(&dir).unwrap()))
    }

    #[test]
    fn a_plain_answer_finishes_in_one_step() {
        let model = Scripted::new(vec![text("all done")]);
        let mut events = Vec::new();
        let outcome = run_agent(
            &model,
            &registry(),
            "do it",
            &AgentConfig::new("m"),
            &mut |e| events.push(e),
        )
        .unwrap();
        assert_eq!(outcome.text, "all done");
        assert_eq!(outcome.steps, 1);
        assert_eq!(outcome.tool_calls, 0);
        assert_eq!(outcome.stop, StopCause::Answer);
        assert!(matches!(
            events.last(),
            Some(Event::Finished {
                reason: StopCause::Answer,
                ..
            })
        ));
    }

    #[test]
    fn a_tool_call_is_executed_and_its_result_goes_back_to_the_model() {
        let model = Scripted::new(vec![
            tool_call(
                "t1",
                "write_file",
                serde_json::json!({"path": "x.txt", "content": "hello"}),
            ),
            text("wrote it"),
        ]);
        let outcome = run_agent(
            &model,
            &registry(),
            "write x",
            &AgentConfig::new("m"),
            &mut |_| {},
        )
        .unwrap();

        assert_eq!(outcome.text, "wrote it");
        assert_eq!(outcome.tool_calls, 1);
        assert_eq!(model.calls(), 2);

        // The second request must contain the assistant tool_use *and* its result.
        let second = &model.requests()[1];
        assert!(
            second.messages.iter().any(|m| m.role == Role::Tool),
            "no tool message"
        );
        let result = second
            .messages
            .iter()
            .flat_map(|m| m.content.iter())
            .find_map(|c| match c {
                Content::ToolResult {
                    tool_use_id,
                    is_error,
                    ..
                } => Some((tool_use_id.clone(), *is_error)),
                _ => None,
            })
            .expect("a tool result");
        assert_eq!(result.0, "t1");
        assert!(!result.1);
    }

    #[test]
    fn usage_accumulates_and_tools_are_declared_but_never_streamed() {
        let model = Scripted::new(vec![
            tool_call("t1", "list_dir", serde_json::json!({})),
            text("done"),
        ]);
        let outcome = run_agent(
            &model,
            &registry(),
            "look",
            &AgentConfig::new("m"),
            &mut |_| {},
        )
        .unwrap();
        // max of the reported values, not the sum: providers report cumulative usage.
        assert_eq!(outcome.usage.input_tokens, 20);
        let first = &model.requests()[0];
        assert!(!first.tools.is_empty(), "tools must be declared");
        assert_eq!(
            first.stream,
            Some(false),
            "the loop wants complete responses"
        );
        assert!(first.tools.iter().any(|t| t.name == "write_file"));
    }

    #[test]
    fn a_failing_tool_is_reported_to_the_model_without_ending_the_run() {
        let model = Scripted::new(vec![
            tool_call(
                "t1",
                "read_file",
                serde_json::json!({"path": "missing.txt"}),
            ),
            text("that file does not exist"),
        ]);
        let mut results = Vec::new();
        let outcome = run_agent(
            &model,
            &registry(),
            "read",
            &AgentConfig::new("m"),
            &mut |e| {
                if let Event::ToolResult { ok, summary, .. } = e {
                    results.push((ok, summary));
                }
            },
        )
        .unwrap();
        assert_eq!(outcome.stop, StopCause::Answer);
        assert_eq!(results.len(), 1);
        assert!(!results[0].0, "the failure must be visible in events");
        assert!(results[0].1.contains("does not exist"), "{}", results[0].1);
    }

    #[test]
    fn an_unknown_tool_comes_back_as_an_error_result() {
        let model = Scripted::new(vec![
            tool_call("t1", "launch_rocket", serde_json::json!({})),
            text("ok"),
        ]);
        let outcome = run_agent(
            &model,
            &registry(),
            "go",
            &AgentConfig::new("m"),
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(outcome.tool_calls, 1);
        let second = &model.requests()[1];
        let text_of_result = second
            .messages
            .iter()
            .flat_map(|m| m.content.iter())
            .find_map(|c| match c {
                Content::ToolResult {
                    content, is_error, ..
                } => Some((
                    content
                        .first()
                        .and_then(|c| c.as_text())
                        .unwrap_or("")
                        .to_string(),
                    *is_error,
                )),
                _ => None,
            })
            .unwrap();
        assert!(text_of_result.1);
        assert!(
            text_of_result.0.contains("no tool named"),
            "{}",
            text_of_result.0
        );
    }

    #[test]
    fn the_step_limit_stops_a_model_that_never_answers() {
        let endless: Vec<CanonicalResponse> = (0..5)
            .map(|i| tool_call(&format!("t{i}"), "list_dir", serde_json::json!({})))
            .collect();
        let model = Scripted::new(endless);
        let cfg = AgentConfig {
            max_steps: 3,
            ..AgentConfig::new("m")
        };
        let outcome = run_agent(&model, &registry(), "loop", &cfg, &mut |_| {}).unwrap();
        assert_eq!(outcome.stop, StopCause::StepLimit);
        assert_eq!(outcome.steps, 3);
        assert_eq!(model.calls(), 3);
    }

    #[test]
    fn the_tool_limit_stops_a_run_and_still_answers_outstanding_calls() {
        let script: Vec<CanonicalResponse> = (0..4)
            .map(|i| tool_call(&format!("t{i}"), "list_dir", serde_json::json!({})))
            .collect();
        let model = Scripted::new(script);
        let cfg = AgentConfig {
            max_tool_calls: 2,
            max_steps: 10,
            ..AgentConfig::new("m")
        };
        let outcome = run_agent(&model, &registry(), "loop", &cfg, &mut |_| {}).unwrap();
        assert_eq!(outcome.stop, StopCause::ToolLimit);
        assert_eq!(outcome.tool_calls, 2);
    }

    #[test]
    fn a_model_error_is_returned_rather_than_swallowed() {
        struct Broken;
        impl Model for Broken {
            fn complete(&self, _: &CanonicalRequest) -> Result<CanonicalResponse, ModelError> {
                Err(ModelError::Refused {
                    status: 429,
                    detail: "slow down".into(),
                })
            }
        }
        let err = run_agent(
            &Broken,
            &registry(),
            "hi",
            &AgentConfig::new("m"),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(err.to_string().contains("429"), "{err}");
        assert!(err.to_string().contains("slow down"), "{err}");
    }

    #[test]
    fn a_tool_use_with_no_tools_registered_is_a_configuration_error() {
        let model = Scripted::new(vec![tool_call(
            "t1",
            "read_file",
            serde_json::json!({"path": "x"}),
        )]);
        let err = run_agent(
            &model,
            &ToolRegistry::new(),
            "hi",
            &AgentConfig::new("m"),
            &mut |_| {},
        )
        .unwrap_err();
        assert!(matches!(err, AgentError::NoTools { .. }), "{err}");
    }
}
