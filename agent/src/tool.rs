//! The tool contract.
//!
//! A tool is a named capability with a JSON Schema and a body that **cannot fail the
//! agent**. Every outcome — including "that file does not exist" — is data handed back to
//! the model, because a model that never sees its own mistake cannot correct it. Only a
//! panic or a broken registry ends a run; the tools themselves always answer.
//!
//! `invoke` takes `&Value` rather than typed arguments on purpose: arguments arrive from a
//! model as JSON, they are frequently wrong, and a typed deserialisation error at the
//! boundary would be indistinguishable from a crash. Parsing happens per-field inside each
//! tool, so the error message can say what was expected.

use std::sync::Arc;

use serde_json::Value;
use zenithal_providers::ToolSpec;

use crate::sandbox::Sandbox;

/// What a tool returns to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    pub content: String,
    /// The model is told this failed, and is expected to try something else.
    pub is_error: bool,
}

impl ToolOutcome {
    pub fn ok(content: impl Into<String>) -> Self {
        ToolOutcome {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn err(content: impl Into<String>) -> Self {
        ToolOutcome {
            content: content.into(),
            is_error: true,
        }
    }

    /// A one-line summary for the UI and the transcript, never the whole payload.
    pub fn summary(&self, max: usize) -> String {
        let first = self.content.lines().next().unwrap_or("").trim();
        let mut s: String = first.chars().take(max).collect();
        if first.chars().count() > max {
            s.push('…');
        }
        s
    }
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// JSON Schema for the argument object.
    fn schema(&self) -> Value;
    fn invoke(&self, args: &Value) -> ToolOutcome;
}

/// The set of tools one agent may use. Each agent gets its own registry, rooted at its own
/// worktree — that is what makes concurrent agents safe.
pub struct ToolRegistry {
    tools: Vec<Box<dyn Tool>>,
}

impl Default for ToolRegistry {
    fn default() -> Self {
        ToolRegistry::new()
    }
}

impl std::fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolRegistry")
            .field("tools", &self.names())
            .finish()
    }
}

impl ToolRegistry {
    pub fn new() -> Self {
        ToolRegistry { tools: Vec::new() }
    }

    /// Read-only tools: the safe default when a caller wants an agent that can look but
    /// not touch.
    pub fn readonly(sandbox: Arc<Sandbox>) -> Self {
        let mut registry = ToolRegistry::new();
        registry.add(Box::new(crate::tools::fs::ReadFile::new(sandbox.clone())));
        registry.add(Box::new(crate::tools::fs::ListDir::new(sandbox.clone())));
        registry.add(Box::new(crate::tools::fs::Search::new(sandbox.clone())));
        registry.add(Box::new(crate::tools::git::GitStatus::new(sandbox.clone())));
        registry.add(Box::new(crate::tools::git::GitDiff::new(sandbox)));
        registry
    }

    /// Read + write, no shell. What a squad member gets by default: it can change files
    /// in its own worktree and nothing else.
    pub fn editing(sandbox: Arc<Sandbox>) -> Self {
        let mut registry = ToolRegistry::readonly(sandbox.clone());
        registry.add(Box::new(crate::tools::fs::WriteFile::new(sandbox.clone())));
        registry.add(Box::new(crate::tools::fs::EditFile::new(sandbox)));
        registry
    }

    /// Everything, including a shell restricted by policy. Opt-in, because a shell is the
    /// difference between "edits my branch" and "runs on my machine".
    pub fn with_shell(
        mut self,
        sandbox: Arc<Sandbox>,
        policy: crate::tools::shell::ShellPolicy,
    ) -> Self {
        self.add(Box::new(crate::tools::shell::RunCommand::new(
            sandbox, policy,
        )));
        self
    }

    pub fn add(&mut self, tool: Box<dyn Tool>) {
        // Replacing by name keeps `add` idempotent and lets a caller override a built-in
        // without the model seeing two tools with the same name.
        self.tools.retain(|t| t.name() != tool.name());
        self.tools.push(tool);
    }

    pub fn remove(&mut self, name: &str) {
        self.tools.retain(|t| t.name() != name);
    }

    pub fn names(&self) -> Vec<&'static str> {
        self.tools.iter().map(|t| t.name()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name() == name)
    }

    /// The declarations sent to the model, sorted by name so the request is stable across
    /// runs (stable prefixes are what provider prompt caches key on).
    pub fn specs(&self) -> Vec<ToolSpec> {
        let mut specs: Vec<ToolSpec> = self
            .tools
            .iter()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: Some(t.description().to_string()),
                input_schema: t.schema(),
            })
            .collect();
        specs.sort_by(|a, b| a.name.cmp(&b.name));
        specs
    }

    /// Run a tool. Unknown names and malformed arguments come back as error outcomes: the
    /// model gets a chance to fix itself, and the run continues.
    pub fn invoke(&self, name: &str, args: &Value) -> ToolOutcome {
        match self.tools.iter().find(|t| t.name() == name) {
            Some(tool) => {
                if !args.is_object() && !args.is_null() {
                    return ToolOutcome::err(format!(
                        "arguments for `{name}` must be a JSON object, got {}",
                        kind_of(args)
                    ));
                }
                tool.invoke(args)
            }
            None => ToolOutcome::err(format!(
                "no tool named `{name}`. available: {}",
                self.names().join(", ")
            )),
        }
    }
}

pub(crate) fn kind_of(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Argument helpers. Every accessor returns a readable error outcome rather than panicking,
/// because the caller is a language model.
pub(crate) fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a Value> {
    args.get(key).filter(|v| !v.is_null())
}

pub(crate) fn required_str<'a>(
    args: &'a Value,
    key: &str,
    tool: &str,
) -> Result<&'a str, ToolOutcome> {
    match arg(args, key) {
        Some(Value::String(s)) => Ok(s),
        Some(other) => Err(ToolOutcome::err(format!(
            "`{key}` must be a string, got {}",
            kind_of(other)
        ))),
        None => Err(ToolOutcome::err(format!(
            "`{tool}` needs a `{key}` argument"
        ))),
    }
}

pub(crate) fn optional_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    match arg(args, key) {
        Some(Value::String(s)) => Some(s),
        _ => None,
    }
}

pub(crate) fn optional_u64(args: &Value, key: &str) -> Option<u64> {
    arg(args, key).and_then(|v| v.as_u64())
}

pub(crate) fn optional_bool(args: &Value, key: &str) -> bool {
    arg(args, key).and_then(|v| v.as_bool()).unwrap_or(false)
}

/// A JSON-Schema object with the given required string properties.
pub(crate) fn schema_object(props: &[(&str, &str, &str)], required: &[&str]) -> Value {
    let mut properties = serde_json::Map::new();
    for (name, kind, description) in props {
        let mut property = serde_json::Map::new();
        property.insert("type".into(), Value::String((*kind).to_string()));
        property.insert(
            "description".into(),
            Value::String((*description).to_string()),
        );
        properties.insert((*name).to_string(), Value::Object(property));
    }
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl Tool for Echo {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn description(&self) -> &'static str {
            "repeat the input"
        }
        fn schema(&self) -> Value {
            schema_object(&[("text", "string", "what to repeat")], &["text"])
        }
        fn invoke(&self, args: &Value) -> ToolOutcome {
            match required_str(args, "text", "echo") {
                Ok(t) => ToolOutcome::ok(t.to_string()),
                Err(e) => e,
            }
        }
    }

    fn registry() -> ToolRegistry {
        let mut r = ToolRegistry::new();
        r.add(Box::new(Echo));
        r
    }

    #[test]
    fn an_unknown_tool_is_an_error_outcome_listing_what_exists() {
        let out = registry().invoke("nope", &serde_json::json!({}));
        assert!(out.is_error);
        assert!(out.content.contains("available: echo"), "{}", out.content);
    }

    #[test]
    fn malformed_arguments_come_back_as_errors_not_panics() {
        let out = registry().invoke("echo", &serde_json::json!({}));
        assert!(out.is_error);
        assert!(out.content.contains("needs a `text` argument"));
    }

    #[test]
    fn non_object_arguments_are_rejected_before_reaching_the_tool() {
        let out = registry().invoke("echo", &serde_json::json!("just a string"));
        assert!(out.is_error);
        assert!(
            out.content.contains("must be a JSON object"),
            "{}",
            out.content
        );
    }

    #[test]
    fn specs_are_sorted_and_carry_their_schema() {
        let specs = registry().specs();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "echo");
        assert_eq!(specs[0].input_schema["required"][0], "text");
        assert!(specs[0].description.is_some());
    }

    #[test]
    fn adding_a_tool_twice_replaces_it_instead_of_shadowing_it() {
        let mut r = registry();
        r.add(Box::new(Echo));
        assert_eq!(r.len(), 1);
        assert_eq!(r.names(), vec!["echo"]);
    }

    #[test]
    fn a_summary_is_one_line_and_bounded() {
        let out = ToolOutcome::ok("first line\nsecond line");
        assert_eq!(out.summary(40), "first line");
        assert_eq!(ToolOutcome::ok("x".repeat(100)).summary(10), "xxxxxxxxxx…");
    }
}
