# Configuration

For basic configuration instructions, see [this documentation](https://developers.openai.com/codex/config-basic).

For advanced configuration instructions, see [this documentation](https://developers.openai.com/codex/config-advanced).

For a full configuration reference, see [this documentation](https://developers.openai.com/codex/config-reference).

## Lifecycle hooks

Admins can set top-level `allow_managed_hooks_only = true` in
`requirements.toml` to ignore user, project, and session hook configs while
still allowing managed hooks from requirements and managed config layers. This
setting is only supported in `requirements.toml`; putting it in `config.toml`
does not enable managed-hooks-only mode.

## MultiDex providers

MultiDex registers an `openrouter` provider by default: it points at
`https://openrouter.ai/api/v1` and reads its credential only from
`experimental_bearer_token` in `CODEX_HOME/config.toml` — provider API keys
never come from environment variables. No keys are stored in the repo.

```toml
[model_providers.openrouter]
experimental_bearer_token = "sk-or-..."
```

OpenRouter provider routing is configured through the `provider` table on the
provider entry and is forwarded verbatim as the request's top-level `provider`
object:

```toml
model_provider = "openrouter"
model = "deepseek/deepseek-v4.1-flash-1017"

[model_providers.openrouter.provider]
only = ["wafer/mxfp4"]
allow_fallbacks = false
```

The bundled catalog already includes `deepseek/deepseek-v4.1-flash-1017`
(context window, reasoning levels, and its DeepSeek-tuned system prompt), so
models served through OpenRouter work without a remote model catalog.

Selecting this slug chooses the built-in `openrouter` provider at startup or
during a live model switch. Selecting `DS v4.1 Flash 1017` chooses the local
provider; selecting a bundled OpenAI model from OpenRouter returns to OpenAI.
Managed provider restrictions still apply.

Both DS entries default to Max reasoning and use the same prompt adapted from
DeepSeek Harness's `snapshots/session/worktree/system-prompt.expected.md`.
The adaptation replaces Harness-only file, background-job, goal, and delegation
tools with MultiDex equivalents; it is not a byte-for-byte copy. Explicit
user-configured instructions can still override the model's default prompt.

### DeepInfra

Choose **Auto** in MultiDex's `/model` picker, or set:

```toml
model = "deepinfra/deepseek-v4.1-flash-1017-auto"
model_provider = "deepinfra"

[model_providers.deepinfra]
experimental_bearer_token = "<deepinfra-api-key>"
```

Auto starts each request on Flex. For HTTP 429/529 capacity errors, timeouts,
or dropped connections before the first valid SSE response event, MultiDex
tries Priority immediately. If valid SSE data already arrived on that request,
Auto retries Flex three times, waiting 5 seconds before each retry, then tries
Priority once if Flex still fails. The next request starts on Flex again.
Other errors do not trigger failover. Select **Flex** or
**Priority** directly to use a fixed tier instead.

### Lithos

The built-in provider ID is `lithos`. Its credential also comes only from
`experimental_bearer_token` in `CODEX_HOME/config.toml`, never the environment:

```toml
[model_providers.lithos]
experimental_bearer_token = "lith_..."
```

It uses a native transport adapter
for `https://api.lithosai.cloud/v1/chat/completions`, without a local proxy.
MultiDex keeps upstream conversation construction and converts only the HTTP
request and streamed response at the provider boundary.

```toml
model_provider = "lithos"
model = "deepseek-ai/DeepSeek-V4.1-Flash"
model_reasoning_effort = "max"
```

The canonical display name across providers is **DeepSeek v4.1 Flash**.
Provider-specific wire IDs remain distinct:

| Provider   | Speed   | Wire model ID                           |
| ---------- | ------- | --------------------------------------- |
| Local      | Regular | `DS v4.1 Flash 1017`                    |
| OpenRouter | Regular | `deepseek/deepseek-v4.1-flash-1017`     |
| Lithos     | Regular | `deepseek-ai/DeepSeek-V4.1-Flash`       |
| Lithos     | Fast    | `deepseek-ai/DeepSeek-V4.1-Flash-fast`  |
| Lithos     | Ultra   | `deepseek-ai/DeepSeek-V4.1-Flash-ultra` |

All entries select the 1017 revision. Lithos's IDs omit the revision suffix;
do not invent an extra suffix in API requests. Reasoning is `off`, `low`,
`high`, or `max`; `off` maps to Lithos's `none` value. Regular is the default
serving variant; faster variants require an explicit model selection.

The adapter preserves text, reasoning, structured function/custom tool calls,
tool results, cached-token usage, and cancellation. Unsupported Responses-only
items produce a clear error. Chat Completions has no stored Responses-ID
continuation; requests retain the upstream history and rely on Lithos's prefix
cache. Generations are not automatically retried, and compaction uses upstream
local compaction through the same adapter.

### Request size for local Lite endpoints

A MultiDex turn can declare hundreds of tools, and every Responses request
normally repeats the whole tool list. Two per-provider options reduce that
upload. OpenAI and remote providers keep their existing request bodies by
default.

- `request_compression = "auto" | "zstd" | "none"` controls request-body
  compression (`Content-Encoding: zstd`).
  - Unset: the built-in OpenAI provider keeps its existing behavior (zstd only
    with ChatGPT sign-in when request compression is enabled). The MultiDex
    providers served by Lite (`multidex` and `omnigent-multidex-*`) use
    `"auto"`. Every other provider sends plain bodies; set
    `request_compression = "auto"` on another provider that points at Lite.
  - `"auto"` sends zstd while the endpoint accepts it. When the server
    advertises its accepted request codings with an RFC 7694 `Accept-Encoding`
    response header, that list decides. Otherwise, if a compressed request is
    rejected with HTTP 400, 415, or 422, MultiDex resends the same request
    uncompressed right away and keeps sending plain bodies to that endpoint
    for the rest of the process. The turn does not fail because of the
    fallback. A later response advertising `zstd` turns compression back on.
  - `"zstd"` always compresses and `"none"` never does.
- `inherits_tools_on_continuation = "auto" | true | false` (default
  `"auto"`, every Responses provider) lets a `previous_response_id`
  continuation omit `tools` so the endpoint reuses the stored list.
  - `"auto"` omits `tools` only when the response being continued carried
    `responses.tools-inheritance/1` in its `X-Inference-Ultra-Capabilities`
    header (Lite sends it on every `/v1/` response). OpenAI and other servers
    never send it, so their requests are unchanged. The first request on an
    endpoint always declares tools, and a response without the token stops
    omission for the next continuation.
  - `true` always allows omission; `false` never omits.
  - Even when allowed, `tools` is omitted only when the tool list is identical
    to the one sent (or inherited) on the continued turn. First requests,
    changed tools, and the request after any failure send the full list.
    `tool_choice` and `parallel_tool_calls` are always sent.
  - Safety net: when `tools` was omitted, the tools echoed in that response's
    `response.completed` must match the omitted list. A missing or different
    echo stops omission for that endpoint for the rest of the process. The
    check is skipped when the response advertises
    `responses.stream-tools/1;mode=omit`, which carries no echo.
  - If the endpoint rejects a tool-less continuation, MultiDex retries once
    with the full tool list. A `response_not_found` error resends the full
    history and tools without `previous_response_id`.
  - It needs `supports_responses_continuation`, which the `multidex` provider
    enables automatically.

`request_compression = "auto"` also treats a `request-encoding/1` capability
whose `codings` include `zstd` as support for zstd request bodies.

For Lite (Inference-Ultra's Responses server), neither option needs
configuration once Lite advertises its capabilities (Inference-Ultra main at
or after a418c0f9, after a restart). Optional overrides:

```toml
[model_providers.multidex]
# ...existing fields...
# inherits_tools_on_continuation = true # omit unchanged tools even without the capability header
# request_compression = "zstd"          # skip negotiation once Lite decodes zstd request bodies
```
