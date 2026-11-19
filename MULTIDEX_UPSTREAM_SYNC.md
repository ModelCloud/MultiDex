# MultiDex upstream sync

This repository follows `openai/codex` main. Keep upstream implementations in
place when merging; carry MultiDex behavior as small, provider-specific changes.

Compatibility points to preserve:

- Official OpenAI models use their official provider. `DS v4.1 Flash 1017` uses
  the configured `multidex` provider, including after a model switch or resume.
- MultiDex reads its live context capacity and compacts at context minus 4,196
  tokens. A changed capacity can trigger compaction before the next request.
- Mid-turn MultiDex steer cancels only the active local stream and retains the
  turn. Upstream Responses Lite interruption still drains its response.
- A dropped response stream cancels its upstream request. Keep this behavior
  when upstream changes response stream ownership.
- Upstream Guardian context overflow recovery remains available alongside
  MultiDex context overflow recovery.
- Provider `request_compression` overrides the upstream OpenAI-only request
  compression gate in `responses_request_compression`. Unset, OpenAI keeps the
  upstream gate, MultiDex providers negotiate
  (`core/src/request_compression.rs`), and other providers stay plain.
- Provider `inherits_tools_on_continuation` (off by default) omits `tools`
  from HTTP continuations only when the wire tool hash matches the request
  that produced the referenced response; any failure clears that hash.

After merging upstream, inspect these paths first:

- `codex-rs/model-provider-info/` and `codex-rs/models-manager/` for provider
  routing, catalog metadata, and context limits.
- `codex-rs/core/src/session/turn.rs` and `codex-rs/core/src/client.rs` for
  stream cancellation, model switching, and compaction.
- `codex-rs/codex-api/src/common.rs` for response stream ownership.

Use upstream conflict hunks for unrelated features. Compile the CLI, app
server, core, and TUI, then run the focused MultiDex model, provider switch,
stream interruption, and compaction tests before publishing a sync.
