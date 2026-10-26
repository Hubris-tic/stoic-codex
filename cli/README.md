# `stoic-codex` — the command

The wiring crate. It parses arguments, finds the local config, opens a memory backend, and
prints. It deliberately owns no logic of its own: wire formats live in `stoic-codex-providers`,
ranking and storage live in `stoic-codex-memory`. Both are independently testable, and both are
tested without this crate.

```bash
cargo build                      # the `stoic-codex` binary, memory via the file store
cargo build --features typedb    # same, plus the local TypeDB backend
cargo test                       # offline suite; live provider checks are ignored by default
cargo test --test net_live -- --ignored   # 2 live TLS tests, needs network
cargo test --test live_gemini -- --ignored # 2 live Gemini tests, needs GEMINI_API_KEY
```

## Everything is local

There is no hosted backend, no account, no telemetry, and no network call in this crate
except the one you configure yourself. Memory has three backends and picks the best one
available on *your* machine:

| Backend | When | Survives restart |
|---|---|---|
| `typedb` | a local TypeDB daemon is present, or can be started | yes |
| `file` | no TypeDB; a JSON file under `~/.stoic-codex/` | yes |
| `memory` | `--backend memory` | no |

`backend` in the config defaults to `auto`: try the daemon, fall back to the file store.
`auto` only falls back when no daemon can be *found* — a daemon that exists but fails is a
real error and is reported rather than silently swapping your data store.

A daemon started by a command is stopped when the command exits, so nothing keeps running
in the background. Warm start plus a recall is about 0.2 s on a 2-core box.

## Config

`~/.stoic-codex/config.json` (override the home with `STOIC_CODEX_HOME`). Every key is optional; a
misspelled key is a hard error, not a silent no-op.

```json
{
  "memory": {
    "backend": "auto",
    "daemon_address": "127.0.0.1:1729",
    "database": "zenithal_memory",
    "context_budget": 1200,
    "allow_daemon": true
  },
  "providers": {
    "default": "gemini",
    "model": "gemini-3.1-flash-lite",
    "api_key_env": "GEMINI_API_KEY",
    "max_tokens": 1024,
    "temperature": 0.2,
    "stream": true,
    "timeout_secs": 120,
    "max_attempts": 4,
    "base_url": null
  }
}
```

On the first non-help CLI command, an existing `~/.zenithal/` config directory is moved to
`~/.stoic-codex/` if the new directory does not exist. If both exist, the new directory wins;
the legacy copy is preserved and a warning is printed. `ZENITHAL_HOME` and `ZENITHAL_PROVIDER`
remain lower-priority compatibility aliases; prefer `STOIC_CODEX_HOME` and
`STOIC_CODEX_PROVIDER` for new setups. The `zenithal_memory` TypeDB database name is retained
so existing local data remains addressable.

`providers.base_url` reroutes every request to a different origin while keeping the
provider's own path — for a local gateway, a proxy, or a test server. The provider
definition itself is untouched.

A non-loopback `daemon_address` is refused before any socket is opened: the memory layer
is for your machine only.

## Commands

### Run a turn

```bash
stoic-codex run "explain what src/auth.rs does"        # streamed answer on stdout
stoic-codex run "..." --provider lab --model tiny      # pick the provider per call
stoic-codex run "..." --no-stream                      # one request, one JSON reply
stoic-codex run "..." --dry-run                        # print the exact request, send nothing
stoic-codex run "..." --json                           # one JSON object: text, usage, cost
stoic-codex run "..." --max-attempts 1                 # fail fast instead of retrying
stoic-codex run "..." --no-session --no-memory --no-remember  # a stateless turn
```

A turn is: recall from memory → compose the system prompt → translate through the
provider's adapter → put it on the socket → decode the stream → print → write the exchange
back to memory. The answer goes to **stdout**, the accounting (provider, model, tokens,
latency, rate-card cost estimate) to **stderr**, so `stoic-codex run "…" > answer.md` gives you a clean file.
The estimate is not billing data; JSON output labels it with `cost_basis: registry_rate_estimate`.

Provider requests use four attempts by default, configurable with `providers.max_attempts`.
Pass `--max-attempts N` to override that limit for one `run`, `agent run`, `mission plan/run`,
`providers models`, or `providers test` invocation. `N` must be 1–10; `1` disables retries.

By default, every successful `run` turn is also saved as a local conversation transcript. The
session ID appears in the stderr summary and in JSON output as `session_id`:

```bash
stoic-codex run "explain the auth module"                         # starts a saved conversation
stoic-codex run "what would you change?" --continue                # continue the newest session
stoic-codex run "make the change" --resume s-...                   # resume a specific session ID
stoic-codex run "one-off question" --no-session                    # do not read or write a transcript
```

Transcripts live in `~/.stoic-codex/sessions/` (or under `$STOIC_CODEX_HOME`) as JSON files. On Unix,
the session directory is mode `0700` and files are mode `0600`. They contain only canonical
user/assistant text plus the provider/model identifiers—not credentials, raw HTTP requests,
the system prompt, or recalled memory. Treat the prompts and answers as sensitive local data.
The system prompt and memory context are rebuilt for every request. When the estimated input plus
the reserved output exceeds a model's context window, Stoic Codex drops the oldest complete turns
from the request copy first; the saved transcript remains complete. If the current task, system
prompt, and output reserve alone do not fit, the request is refused before any provider request.


The provider is chosen from `--provider`, then `$STOIC_CODEX_PROVIDER`, then
`providers.default`; the model from `--model`, then `providers.model`. If neither is set
the error lists the built-in ids instead of guessing.

Credentials come from the environment, never from the config file: `<PROVIDER>_API_KEY`
(e.g. `GEMINI_API_KEY`), or whatever `providers.api_key_env` names. The example uses Google
AI Studio; free-tier availability and limits depend on the account/model, so check your own quota.
The cost line is a rate-card estimate, not proof of an actual charge. A local endpoint
that wants no key says so in its own spec (`"auth": {"kind": "none"}`), and then none is
required.

#### Trying it with no model at all

`dev/stub_provider.py` is a fake OpenAI-compatible server that streams a canned answer, so
you can exercise the whole path before downloading a single weight:

```bash
python3 dev/stub_provider.py --port 8099 &
cat > /tmp/lab.json <<'JSON'
{ "id": "lab", "base_url": "http://127.0.0.1:8099", "api_style": "openai",
  "models": ["tiny"], "auth": { "kind": "none" } }
JSON
stoic-codex providers add /tmp/lab.json
stoic-codex providers test lab --model tiny     # one ping: status, latency, tokens, reply
stoic-codex run "hello" --provider lab --model tiny
```

`dev/stub_mission.py` is the same idea for the multi-agent path: it plays Strategos and the
operators of The Phalanx so a whole mission can be watched without a key. `--lazy` makes every
operator answer without touching a file, which is how the failure path is rehearsed. See
`../agent/MISSION-DEMO.md` for a verbatim run.

### Against a live provider

Verified on 2026-10-26 against the real Gemini API — the first live vendor work in this
project, and the reason two adapter bugs are now fixed ([`../providers/LIVE.md`](../providers/LIVE.md)):

```
$ stoic-codex run "Reply with exactly: live through stoic-codex" --provider gemini --model gemini-3.1-flash-lite
live through stoic-codex
gemini · gemini-3.1-flash-lite · in 207 out 4 tok · 794 ms · rate-card estimate $0.000058 (not billing data)

$ stoic-codex agent run "edit hash() to call sha256_hex" --repo . --provider gemini --model gemini-3.1-flash-lite
→ read_file  {"path":"src/auth.rs"}
  ok read_file (0ms)
→ edit_file  {"old":"…","new":"…"}
  ok edit_file (0ms) edited src/auth.rs (1 replacement)
→ read_file  {"path":"src/auth.rs"}
  ok read_file (0ms)
— answered, 3 tool calls, 119 chars
```

Four turns, three tool calls, a real file changed on disk. The key is read from
`providers.api_key_env` (here `GEMINI_API_KEY`) and never appears in config, in a request log,
or in an error message. This was a Google AI Studio call under the stated free-tier constraint;
the printed price is only the list-rate estimate and is not billing-console data. No Anthropic or
OpenAI live calls are required. To reproduce the checks (only if the key is within your free quota):

```bash
export GEMINI_API_KEY=…
cargo test --test live_gemini -- --ignored --test-threads=1
```

### Agents (tools, sandbox, repository)

```bash
stoic-codex agent run "make the tests pass" --repo .               # one agent, editing tools
stoic-codex agent run "why does build.sh fail?" --read-only        # read tools only
stoic-codex agent run "run the suite and fix what breaks" \
   --allow-shell cargo,git --max-steps 20                     # explicit shell allow-list
stoic-codex tools                                                  # what an agent can do
stoic-codex tools --json                                           # ...as the JSON Schema the model sees
```

Tools are confined to the repository by a `Sandbox`: absolute paths, `..`, symlink escapes
and `.git` writes are refused. `--allow-shell` is an allow-list of program names — there is
no shell, no pipes, and no shell metacharacters are interpreted.

### Mission (Strategos plans, The Phalanx works)

```bash
stoic-codex mission plan "add per-file retry budgets"      # one call: what Strategos would assign
stoic-codex mission run  "add per-file retry budgets"      # 3 operators in 3 git worktrees
stoic-codex mission run  "…" --agents 5 --parallel 3 --verify "cargo test"
stoic-codex mission run  "…" --no-review --no-commit       # inspect before anything lands
stoic-codex mission run  "…" --json --quiet                # machine-readable report, no progress
```

One boss — **Strategos**, no tools — plans the split, names each operator, then judges what
comes back. Each operator gets a short-hand system code (`x1-write`, `x2-check`, …), its own
git worktree, branch (`stoic-codex/<code>`), sandbox and model client. Accepted files are copied
into the working tree whole, first accepted writer wins, and the mission makes one commit.
Overlapping edits are reported as conflicts, never merged; a rejected operator keeps its work
on its branch. Exit code is `1` when nothing applicable landed, even if every model answered
politely.

The progress log on stderr is a fixed-column table — `[code]`, a verb, then `key=value` — so a
mission can be watched while three operators type at once:

```
[strategos] plan    reading the layout
[strategos] name    slot-1     → x1-write · the primary logic generator
[strategos] deploy  x1-write     branch=stoic-codex/x1-write
[x1-write]  tool    write_file       {"path":"src/auth.rs"}
[x1-write]  return  state=done
[strategos] done    applied=1 conflicts=0
```

`mission plan --json` prints `{"source","commander","phalanx","assignments":[{"code","task"}]}`
and touches nothing.

See `../agent/README.md` for the diagram and the degradation rules.

### Providers (BYOK)

```bash
stoic-codex providers ids                    # every built-in id
stoic-codex providers list                   # built-ins + your own specs
stoic-codex providers add ./lab.json         # validate, then install to ~/.stoic-codex/providers/
stoic-codex providers show gemini            # request shape; no network call
stoic-codex providers models gemini           # live GET /v1beta/models; no completion
stoic-codex providers models openrouter --free # only explicit `:free` / zero-price models
stoic-codex providers models groq --max-attempts 1 # bounded, read-only catalogue retries
stoic-codex providers show lab --model m1     # ...for a provider you wrote yourself
stoic-codex providers test lab --model m1 --max-attempts 1 # completion request; use only on free/local
```

`providers show` renders the credential as `<credential>` and never opens a socket. `providers
models` performs a read-only catalogue GET (no prompt or generated tokens); `run`, `agent run`,
`mission run` and `providers test` send completion requests. Only test providers whose free quota
you have confirmed. `FREE` is shown only for an explicit model suffix or two explicit zero prices;
missing price/quota metadata means **unknown**, not free. Google AI Studio and Groq may not publish
free-tier eligibility in their model lists.

A spec you install shadows a built-in with the same id — your definition is the more
specific statement of intent. See `../providers/CUSTOM-PROVIDERS.md`.

### Memory

```bash
stoic-codex memory init                      # create/open the store, report which backend
stoic-codex memory remember file:auth.rs auth.rs --kind file --text "token verification"
stoic-codex memory remember note:design design.md --from ./design.md --importance 0.8
stoic-codex memory link commit:4f2c file:auth.rs fixes
stoic-codex memory recall "token expiry" --focus file:auth.rs --limit 5
stoic-codex memory why "token expiry"        # score breakdown per hit
stoic-codex memory forget note:design        # drop it and its edges
stoic-codex memory stats --json
```

Kinds: `file`, `symbol`, `commit`, `task`, `decision`, `error`, `agent`, `doc`.
Edge kinds: `calls`, `imports`, `fixes`, `regresses`, `blocks`, `supersedes`,
`derived_from`, `authored_by`.

`recall` prints `score  [kind]  label  id`. `why` prints the same rows with the signals
that produced the score (`cos`, `bm25`, `graph`, `rec`, `imp`) and the traversal path when
a memory was reached through the graph rather than by text.

### Context

```bash
stoic-codex context "token expiry"                 # the prompt block, system + user
stoic-codex context "token expiry" --budget 600
stoic-codex context "token expiry" --system-only   # drop-in system prompt
stoic-codex context "token expiry" --json
```

This is where memory stops being a database. The block is ordered identity → memory →
task, and retrieved text is fenced in `<memory>` with an explicit instruction that it may
be stale and must never be treated as instructions. `--budget` caps the block in
characters; entries are never cut mid-way, and if the "N more entries omitted" footer
does not fit, the footer goes, not the memory.

### Daemon

```bash
stoic-codex daemon status                    # read-only; never starts anything
```

### Exit codes

`0` ok · `1` failed (the operation was understood and could not be completed) ·
`2` usage (the request was malformed). Scripts can tell "you asked something impossible"
from "the tool is unhappy".

## Network

The only I/O in this tree lives in `src/net/` (~400 lines, no async runtime):

* `http` and `https` — TLS through rustls and the **system trust store**. Local-first does
  not mean plaintext: the moment the endpoint is a hosted provider, the connection is
  still authenticated, or BYOK would mean mailing your key in the clear.
* `Content-Length`, chunked, and read-to-EOF bodies; one request per connection.
* The body is handed to the caller **as it arrives**, which is what makes streaming work.
* No HTTP/2, no redirects (a 3xx is reported, not followed), no proxies of its own, no
  gzip.

## Design notes

- **No CLI framework.** Parsing is ~100 lines with tests. Same reason the provider layer
  hand-rolls SSE: a flat dependency tree and a fast start.
- **Commands never call `process::exit`.** They return a typed `CmdError`; `run()` maps it
  to a code. That is what makes 28 end-to-end tests possible with an injected `$HOME` and
  captured output, and it is why nothing here touches the real home directory in tests.
- **Errors name the file.** A broken `config.json` is reported with its path, and a corrupt
  memory file is refused rather than overwritten.
- **Empty is not an error.** No recall matches prints why ("no memories matched `x`
  (backend file, 12 entries)") and exits 0.
- **A failed turn changes nothing.** Memory is written only after a successful response,
  so a 429 cannot poison recall, and a retry does not leave half an exchange behind.
- **The adapter is the only vendor-aware code.** `run.rs` builds a canonical request and
  decodes canonical events; it never reads a vendor JSON field. That is why one code path
  drives Anthropic, OpenAI, Gemini, a local `llama.cpp` server, and a provider written by
  the user in JSON.
