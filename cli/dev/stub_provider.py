#!/usr/bin/env python3
"""A fake OpenAI-compatible server, so `stoic-codex run` can be exercised with no model at all.

It streams a canned answer token by token, which is exactly what a local model does — so
you can check streaming, usage accounting, memory write-back and `--dry-run` without
downloading a single weight.

    python3 cli/dev/stub_provider.py --port 8099

Then, in another terminal:

    cat > ~/.stoic-codex/providers/lab.json <<'JSON'
    {
      "id": "lab",
      "base_url": "http://127.0.0.1:8099",
      "api_style": "openai",
      "models": ["tiny"],
      "auth": { "kind": "none" }
    }
    JSON

    stoic-codex providers add ~/.stoic-codex/providers/lab.json
    stoic-codex run "what does my memory say about auth?" --provider lab --model tiny

Every request is echoed to stderr so you can see exactly what the binary sent.
"""

import argparse
import json
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ANSWER = (
    "This is a stub answer from a local server. It arrives as several SSE deltas, "
    "which is how a real streaming endpoint behaves."
)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def do_POST(self):  # noqa: N802 (stdlib naming)
        length = int(self.headers.get("content-length", "0"))
        body = self.rfile.read(length)
        try:
            parsed = json.loads(body or b"{}")
        except json.JSONDecodeError:
            parsed = {"_unparsed": body.decode("utf-8", "replace")}

        print(f"\n--- {self.command} {self.path}", flush=True)
        for k, v in self.headers.items():
            shown = "<redacted>" if k.lower() in ("authorization", "x-api-key") else v
            print(f"    {k}: {shown}", flush=True)
        print("    body: " + json.dumps(parsed)[:600], flush=True)

        model = parsed.get("model", "stub-model")

        # Honour `stream: false` the way a real server does, so both code paths are
        # reachable from this stub — and decide it *before* any header is written.
        if parsed.get("stream") is False:
            payload = {
                "id": "chatcmpl-stub",
                "object": "chat.completion",
                "model": model,
                "choices": [
                    {
                        "index": 0,
                        "message": {"role": "assistant", "content": ANSWER},
                        "finish_reason": "stop",
                    }
                ],
                "usage": {
                    "prompt_tokens": 42,
                    "completion_tokens": len(ANSWER.split(" ")),
                    "total_tokens": 42 + len(ANSWER.split(" ")),
                },
            }
            raw = json.dumps(payload).encode()
            self.send_response(200)
            self.send_header("content-type", "application/json")
            self.send_header("content-length", str(len(raw)))
            self.send_header("connection", "close")
            self.end_headers()
            self.wfile.write(raw)
            self.wfile.flush()
            return

        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()

        words = ANSWER.split(" ")
        for i, word in enumerate(words):
            piece = word if i == 0 else " " + word
            chunk = {
                "id": "chatcmpl-stub",
                "object": "chat.completion.chunk",
                "model": model,
                "choices": [{"index": 0, "delta": {"content": piece}, "finish_reason": None}],
            }
            self.wfile.write(f"data: {json.dumps(chunk)}\n\n".encode())
            self.wfile.flush()
            time.sleep(0.03)

        final = {
            "id": "chatcmpl-stub",
            "object": "chat.completion.chunk",
            "model": model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 42, "completion_tokens": len(words), "total_tokens": 42 + len(words)},
        }
        self.wfile.write(f"data: {json.dumps(final)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()

    def log_message(self, *_args):
        pass


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--port", type=int, default=8099)
    ap.add_argument("--host", default="127.0.0.1")
    args = ap.parse_args()
    server = ThreadingHTTPServer((args.host, args.port), Handler)
    print(f"stub provider listening on http://{args.host}:{args.port}/v1", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
