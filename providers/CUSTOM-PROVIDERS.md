# Custom providers — "the user writes the API"

Two ways to add a provider without touching Rust. Both live in `src/custom.rs` and are
loaded from JSON at runtime.

## 1. Point at an OpenAI-compatible endpoint (`api_style: openai`)

Covers llama.cpp, vLLM, LM Studio, Ollama's OpenAI mode, OpenRouter, Together, and any
internal gateway that speaks the same protocol.

```json
{
  "id": "my-lab",
  "base_url": "http://127.0.0.1:8080",
  "path": "/v1/chat/completions",
  "api_style": "openai",
  "auth": { "kind": "header", "name": "x-api-key", "prefix": "" },
  "api_key_env": "LAB_API_KEY",
  "models": ["my-model-7b"],
  "local": true
}
```

The request **body** is produced by the same code path as the built-in OpenAI adapter —
a test asserts byte equality — so you inherit every fix and quirk that adapter has. Only
the URL, headers, auth placement, and model names come from your spec.

## 2. Describe an API that is not OpenAI-shaped (`api_style: custom`)

```json
{
  "id": "weird-api",
  "base_url": "https://api.example.com",
  "path": "/infer",
  "api_key_env": "EXAMPLE_API_KEY",
  "api_style": "custom",
  "request": {
    "prompt": "{{text}}",
    "system": "{{system}}",
    "max_new_tokens": "{{max_tokens}}",
    "stream": "{{stream}}"
  },
  "response": { "text": "output.text", "usage": { "input": "output.tokens.in", "output": "output.tokens.out" } },
  "stream": { "text_delta": "delta.text", "done_marker": "[EOS]" },
  "stop_reasons": { "cutoff": "max_tokens" }
}
```

### Placeholders

| Placeholder | Meaning |
|---|---|
| `{{model}}` | the model name |
| `{{system}}` | system prompt |
| `{{text}}` | whole conversation flattened to text (for single-prompt APIs) |
| `{{messages}}` | array of `{role, content}` messages |
| `{{tools}}` | array of `{type:"function", function:{name, description, parameters}}` |
| `{{tool_choice}}`, `{{stop}}`, `{{stream}}`, `{{json_mode}}`, `{{max_tokens}}`, `{{temperature}}`, `{{top_p}}` | the obvious values |

A placeholder that is the **entire** string keeps its JSON type — `"max_new_tokens":
"{{max_tokens}}"` becomes the number `4096`, not `"4096"`. A placeholder inside a longer
string is substituted as text. An unknown placeholder is a **load-time error**, because a
typo that renders to `""` produces a 400 that is very hard to trace back.

### Paths

Dotted lookups into the response: `choices.0.delta.content`, `output.tokens.in`.
Array indices are decimal segments. A missing path yields "no data", never a panic; a
missing *required* path (e.g. `response.text`) is an error that names the path you set.

## Secrets

The spec never contains a key. It names an environment variable (`api_key_env`); the
transport resolves and formats it (`AuthSpec::render`), so keys never enter adapter
state, `Debug` output, error strings, or the IR. Validation rejects any `headers` entry
that looks like a credential (`Authorization`, `x-api-key`, …) and tells you to use
`api_key_env` instead.

## Local-only by default

`"local": true` marks a model as running on your machine: free, private, no key needed.
That flag is what the router uses to prefer local endpoints for cheap task classes.

## Validation

`CustomProvider::from_json` rejects, with the offending field named:

- missing or non-`http(s)` `base_url`, or one ending in `/`
- an `id` outside `[a-zA-Z0-9_-]`
- `api_style: custom` with no `request` template
- credential-shaped headers
- empty `api_key_env`
