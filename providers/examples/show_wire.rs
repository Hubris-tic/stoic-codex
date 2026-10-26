//! Inspect exactly what each provider will be sent.
//!
//!   cargo run --example show_wire -- anthropic
//!   cargo run --example show_wire -- all
//!
//! No network, no keys: this prints the request the adapter builds for one
//! canonical transcript, which is the fastest way to debug a 400 from a new vendor.

use stoic_codex_providers::adapters;
use stoic_codex_providers::{CanonicalRequest, Content, Message, Role, ToolSpec};

fn demo_request() -> CanonicalRequest {
    let mut req = CanonicalRequest::new("demo-model", "Open src/main.rs and tell me what it does.")
        .with_system("You are a coding agent. Be terse.")
        .with_max_tokens(2048)
        .with_tools(vec![ToolSpec {
            name: "read_file".into(),
            description: Some("Read a file from the workspace".into()),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"]
            }),
        }]);

    // A completed tool round-trip, which is where provider formats diverge most.
    req.messages.push(Message {
        role: Role::Assistant,
        content: vec![Content::ToolUse {
            id: "call_1".into(),
            name: "read_file".into(),
            input: serde_json::json!({ "path": "src/main.rs" }),
            signature: None,
        }],
    });
    req.messages.push(Message::tools(vec![Content::ToolResult {
        tool_use_id: "call_1".into(),
        content: vec![Content::text("fn main() { println!(\"hi\") }")],
        is_error: false,
    }]));
    req
}

fn main() {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "all".to_string());
    let ids: Vec<&str> = if arg == "all" {
        adapters::provider_ids()
    } else {
        vec![Box::leak(arg.into_boxed_str()) as &str]
    };

    let req = demo_request();
    for id in ids {
        match adapters::by_id(id) {
            Ok(a) => match a.build_request(&req) {
                Ok(w) => {
                    println!("=== {id} ===");
                    println!("{} {}", w.method, w.url);
                    println!(
                        "auth: {:?}   (resolved by the transport, never stored here)",
                        w.auth
                    );
                    for (k, v) in &w.headers {
                        println!("{k}: {v}");
                    }
                    println!("{}", serde_json::to_string_pretty(&w.body).unwrap());
                    println!();
                }
                Err(e) => println!("=== {id} === skipped: {e}\n"),
            },
            Err(e) => println!("=== {id} === unknown provider: {e}\n"),
        }
    }
}
