---
name: multidex-privacy
description: Enforce MultiDex's no-telemetry, no-tracking, no-phone-home rule for the CLI, the TUI, MCP servers, and every process a session launches. Load when touching analytics, OTEL export, feedback, update checks, or any code that spawns a child process.
---

# No telemetry, tracking, or phone-home

MultiDex never sends session data, usage analytics, crash reports, prompts,
transcripts, repository paths, or hostnames to OpenAI or any third party.

## The mechanism

`codex-rs/protocol/src/privacy_env.rs` owns the canonical list:

- `PRIVACY_ENV_VARS` — `DO_NOT_TRACK=1`, `DISABLE_TELEMETRY=1`,
  `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, the Claude Code
  error/auto-update switches, the Node toolchain analytics flags, and the
  update/analytics notices of common CLIs.
- `insert_privacy_env` / `insert_privacy_env_os` — force the values into a
  child environment map.
- `apply_privacy_env` — force them onto a `Command`, overriding inherited
  values.
- Analytics and feedback are hard-disabled in `codex-rs/core/src/config/mod.rs`:
  `analytics_enabled` is always `Some(false)` and `feedback_enabled` is always
  `false`. The `analytics` / `feedback` config tables are inert.

Enforcement points already wired:

| Boundary | Where |
| --- | --- |
| Shell and exec children | `codex-rs/protocol/src/shell_environment.rs` `create_env_from_vars` |
| Every spawned child | `codex-rs/core/src/spawn.rs` `spawn_child_async` |
| Local MCP stdio servers | `codex-rs/rmcp-client/src/utils.rs` `create_env_for_mcp_server` |
| Remote MCP servers | `codex-rs/rmcp-client/src/utils.rs` `create_env_overlay_for_remote_mcp_server` |
| Analytics and feedback | `codex-rs/core/src/config/mod.rs` (always off) |

## Rules

- Never remove or weaken a call to the privacy helpers. An inherited
  `DO_NOT_TRACK=0` must still produce a private child.
- A new spawn path must call one of the helpers. Do not copy the variable list
  into a call site; extend `PRIVACY_ENV_VARS`.
- Never add a telemetry exporter, analytics SDK, session replay, or crash
  upload that is enabled when configuration is missing. Missing means off.
- Never re-enable OpenAI analytics or feedback from configuration. Do not turn
  `analytics.enabled` or `feedback.enabled` into a live switch, and do not add a
  consent flow that turns them on when configuration is missing.
- `otel.metrics_exporter` is the only permitted sink, and only when the operator
  names an explicit collector. Never default it to a vendor endpoint.
- Never add a probe, migration, or daemon start-up path that emits telemetry
  the interactive session would not.
- Do not silence a test that asserts the opt-outs; fix the code instead.

## Verify

```sh
cargo test -p codex-protocol privacy_env
```

```sh
cargo test -p codex-core feedback_enabled_defaults_to_false
cargo test -p codex-core analytics_and_feedback_cannot_be_enabled_from_config
cargo test -p codex-rmcp-client remote_env_overlay
```

For a live check, run any session command and inspect its child environment:

```sh
cargo run -p codex-cli -- exec --skip-git-repo-check 'env | grep -E "DO_NOT_TRACK|CLAUDE_CODE_DISABLE_NONESSENTIAL"'
```
