#!/usr/bin/env python3
"""Exercise one live app-server thread across four loopback providers.

Uses fake credentials and mock HTTP endpoints. Run with --binary /path/to/codex.
The separate opt-in live provider tests use the user's private environment key.
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path


def declared_tool_names(
    tools: list[dict], namespace: str = ""
) -> list[tuple[str, str]]:
    names = []
    for tool in tools:
        if tool.get("type") == "namespace":
            scope = tool["name"] if tool["name"] != "functions" else ""
            names.extend(declared_tool_names(tool["tools"], scope))
        else:
            name = (
                tool.get("name") or tool.get("function", {}).get("name") or tool["type"]
            )
            names.append((namespace, name))
    return names


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    args = parser.parse_args()
    requests: list[tuple[str, dict]] = []
    tokens = {
        "openai": "test-only-openai",
        "multidex": "test-only-local",
        "lithos": "test-only-lithos",
        "openrouter": "test-only-openrouter",
    }

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def do_GET(self):
            # Exercise Codex's documented HTTP fallback for a non-WS API server.
            self.send_error(
                426 if self.headers.get("Upgrade", "").lower() == "websocket" else 404
            )

        def do_POST(self):
            body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
            provider = self.path.split("/")[1]
            assert self.headers.get("Authorization") == "Bearer " + tokens[provider], (
                "credential crossed providers"
            )
            requests.append((self.path, body))
            if provider != "openai":
                names = declared_tool_names(body.get("tools") or [])
                for item in body.get("input", []):
                    if item.get("type") == "additional_tools":
                        names.extend(declared_tool_names(item["tools"]))
                if len(names) != len(set(names)):
                    self.send_error(400, "duplicate tool declarations")
                    return
            rid = f"resp_handoff_{len(requests)}"
            if provider == "lithos":
                assert self.path.endswith("/chat/completions"), self.path
                assert "ORCHID" in json.dumps(body["messages"]), (
                    "conversation history was lost"
                )
                events = [
                    {
                        "id": rid,
                        "choices": [
                            {
                                "index": 0,
                                "delta": {
                                    "reasoning_content": "Recall the original word."
                                },
                                "finish_reason": None,
                            }
                        ],
                    },
                    {
                        "id": rid,
                        "choices": [
                            {
                                "index": 0,
                                "delta": {"content": "ORCHID"},
                                "finish_reason": "stop",
                            }
                        ],
                        "usage": {
                            "prompt_tokens": 1024,
                            "completion_tokens": 10,
                            "total_tokens": 1034,
                            "prompt_tokens_details": {"cached_tokens": 512},
                        },
                    },
                ]
                wire = (
                    "".join("data: " + json.dumps(event) + "\n\n" for event in events)
                    + "data: [DONE]\n\n"
                )
            else:
                assert self.path.endswith("/responses"), self.path
                assert "ORCHID" in json.dumps(body["input"]), (
                    "conversation history was lost"
                )
                if provider == "openrouter":
                    assert body["provider"] == {
                        "only": ["wafer/mxfp4"],
                        "allow_fallbacks": False,
                    }
                if provider == "openai":
                    assert not any(
                        item.get("type") == "reasoning" and item.get("content")
                        for item in body["input"]
                    ), "plaintext reasoning reached OpenAI"
                text = "OPENAI_OK" if provider == "openai" else "LOCAL_OK"
                item = {
                    "type": "message",
                    "id": "msg_" + rid,
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": text}],
                }
                events = [
                    {"type": "response.created", "response": {"id": rid}},
                    {
                        "type": "response.output_item.added",
                        "output_index": 0,
                        "item": {**item, "content": []},
                    },
                    {
                        "type": "response.output_text.delta",
                        "item_id": item["id"],
                        "output_index": 0,
                        "content_index": 0,
                        "delta": text,
                    },
                    {
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": item,
                    },
                    {
                        "type": "response.completed",
                        "response": {
                            "id": rid,
                            "status": "completed",
                            "output": [item],
                            "usage": {
                                "input_tokens": 1024,
                                "output_tokens": 4,
                                "total_tokens": 1028,
                            },
                        },
                    },
                ]
                wire = "".join(
                    "event: " + event["type"] + "\ndata: " + json.dumps(event) + "\n\n"
                    for event in events
                )
            payload = wire.encode()
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    base = f"http://127.0.0.1:{server.server_port}"
    with tempfile.TemporaryDirectory(
        prefix="lithos-handoff-", dir=Path.home()
    ) as directory:
        root = Path(directory)
        home = root / "codex-home"
        home.mkdir()
        (home / "config.toml").write_text(f'''openai_base_url = "{base}/openai/v1"
service_tier = "default"
[model_providers.multidex]
name = "MultiDex"
base_url = "{base}/multidex/v1"
experimental_bearer_token = "{tokens["multidex"]}"
wire_api = "responses"
requires_openai_auth = false
[model_providers.lithos]
base_url = "{base}/lithos/v1"
experimental_bearer_token = "{tokens["lithos"]}"
[model_providers.openrouter]
base_url = "{base}/openrouter/v1"
experimental_bearer_token = "{tokens["openrouter"]}"
[model_providers.openrouter.provider]
only = ["wafer/mxfp4"]
allow_fallbacks = false
''')
        (home / "auth.json").write_text(
            json.dumps({"auth_mode": "apikey", "OPENAI_API_KEY": tokens["openai"]})
        )
        (home / "auth.json").chmod(0o600)
        env = {
            **os.environ,
            "CODEX_HOME": str(home),
            "CODEX_DISABLE_CLOUD_CONFIG": "1",
            "OPENAI_API_KEY": tokens["openai"],
            "CODEX_API_KEY": tokens["openai"],
            "MULTIDEX_API_KEY": "",
            "LITHOS_API_KEY": "",
            "OPENROUTER_API_KEY": "",
        }
        with (root / "stderr.log").open("w") as stderr:
            process = subprocess.Popen(
                [args.binary, "app-server", "--stdio"],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=stderr,
                text=True,
                env=env,
            )
            events: queue.Queue = queue.Queue()

            def reader():
                for line in process.stdout:
                    try:
                        events.put(json.loads(line))
                    except ValueError:
                        events.put({"invalidJson": line})

            threading.Thread(target=reader, daemon=True).start()
            sequence = 0
            context_windows: list[int] = []

            def receive(predicate, seconds=40):
                deadline = time.monotonic() + seconds
                while time.monotonic() < deadline:
                    event = events.get(timeout=max(0.01, deadline - time.monotonic()))
                    if "invalidJson" in event:
                        raise AssertionError("app-server emitted invalid JSON")
                    if event.get("method") == "error":
                        raise AssertionError(event)
                    if event.get("method") == "thread/tokenUsage/updated":
                        context_windows.append(
                            event["params"]["tokenUsage"]["modelContextWindow"]
                        )
                    if predicate(event):
                        return event
                raise TimeoutError("app-server response timed out")

            def rpc(method, params):
                nonlocal sequence
                sequence += 1
                process.stdin.write(
                    json.dumps(
                        {
                            "jsonrpc": "2.0",
                            "id": sequence,
                            "method": method,
                            "params": params,
                        }
                    )
                    + "\n"
                )
                process.stdin.flush()
                reply = receive(lambda event: event.get("id") == sequence)
                assert "error" not in reply, reply
                return reply["result"]

            try:
                rpc(
                    "initialize",
                    {
                        "clientInfo": {"name": "lithos-handoff-test", "version": "1"},
                        "capabilities": {"experimentalApi": True},
                    },
                )
                process.stdin.write('{"jsonrpc":"2.0","method":"initialized"}\n')
                process.stdin.flush()
                started = rpc(
                    "thread/start",
                    {
                        "model": "gpt-5.6-sol",
                        "modelProvider": "openai",
                        "cwd": str(root),
                        "approvalPolicy": "never",
                        "sandbox": "workspace-write",
                    },
                )
                thread_id = started["thread"]["id"]
                local_windows = iter((131_072, 524_288))
                # Every DeepSeek route shares the bundled budget until a local snapshot exists.
                deepseek_window = 262_144
                for provider, model, prompt in [
                    (
                        "openai",
                        "gpt-5.6-sol",
                        "The original secret word is ORCHID. Acknowledge only.",
                    ),
                    (
                        "lithos",
                        "deepseek-ai/DeepSeek-V4.1-Flash",
                        "What was the original secret word? Reply only that word.",
                    ),
                    (
                        "multidex",
                        "DS v4.1 Flash 1017",
                        "Keep the original secret word. Acknowledge only.",
                    ),
                    (
                        "openrouter",
                        "deepseek/deepseek-v4.1-flash-1017",
                        "What was the original secret word? Reply only that word.",
                    ),
                    (
                        "lithos",
                        "deepseek-ai/DeepSeek-V4.1-Flash-ultra",
                        "What was the original secret word? Reply only that word.",
                    ),
                    (
                        "openai",
                        "gpt-5.6-sol",
                        "Keep the original secret word. Acknowledge only.",
                    ),
                    (
                        "openrouter",
                        "deepseek/deepseek-v4.1-flash-1017",
                        "What was the original secret word? Reply only that word.",
                    ),
                    (
                        "multidex",
                        "DS v4.1 Flash 1017",
                        "Keep the original secret word. Acknowledge only.",
                    ),
                    (
                        "openai",
                        "gpt-5.6-sol",
                        "Keep the original secret word. Acknowledge only.",
                    ),
                ]:
                    if provider == "multidex":
                        deepseek_window = next(local_windows)
                        (home / "multidex-runtime-capabilities.json").write_text(
                            json.dumps(
                                {
                                    "model": model,
                                    "context_window": deepseek_window,
                                    "max_prompt_tokens": deepseek_window - 2,
                                }
                            )
                        )
                    expected_window = deepseek_window if provider != "openai" else None
                    if provider != "openai" or requests:
                        # Model-only selection must infer the provider, as /model does.
                        rpc(
                            "thread/settings/update",
                            {
                                "threadId": thread_id,
                                "model": model,
                                "effort": "high",
                                "serviceTier": "default",
                            },
                        )
                    before = len(requests)
                    usage_before = len(context_windows)
                    rpc(
                        "turn/start",
                        {
                            "threadId": thread_id,
                            "input": [
                                {"type": "text", "text": prompt, "text_elements": []}
                            ],
                        },
                    )
                    completed = receive(
                        lambda event: event.get("method") == "turn/completed"
                    )
                    assert completed["params"]["turn"]["status"] == "completed", (
                        completed
                    )
                    assert len(requests) > before
                    assert all(
                        path.startswith("/" + provider + "/") and body["model"] == model
                        for path, body in requests[before:]
                    )
                    assert len(context_windows) > usage_before, (
                        "missing context budget notification"
                    )
                    assert context_windows[-1] > 0
                    if expected_window is not None:
                        assert context_windows[-1] == expected_window, (
                            provider,
                            context_windows[-1],
                            expected_window,
                        )
                    print(
                        "PASS",
                        provider,
                        model,
                        "context:",
                        context_windows[-1],
                        flush=True,
                    )
            finally:
                process.terminate()
                process.wait(timeout=10)
                server.shutdown()
    print(
        "PASS: one thread, nine turns, unique tools and provider credentials/endpoints/history preserved"
    )


if __name__ == "__main__":
    main()
