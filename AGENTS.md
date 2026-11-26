# MultiDex fork guidance

Preserve upstream behavior while keeping MultiDex provider routing, Responses continuation,
compaction safeguards, and portable endpoint tool support. Prefer adapting fork features to
upstream abstractions so later merges remain small.

Upstream owns conversation/context construction and formatting, including instructions,
history items, and compaction payloads. MultiDex adds local endpoint support; do not fork
those formats. Keep endpoint compatibility adaptations at the provider/transport boundary.

## Privacy: no telemetry, tracking, or phone-home

MultiDex sends nothing about a session to OpenAI or any third party. OpenAI
analytics, Statsig metrics, feedback uploads, and update pings are permanently
disabled: `analytics_enabled` is always `Some(false)` and `feedback_enabled` is
always `false`, regardless of `config.toml`. Every child process is forced to
carry the opt-outs in
[codex-rs/protocol/src/privacy_env.rs](codex-rs/protocol/src/privacy_env.rs).
See [.codex/skills/multidex-privacy/SKILL.md](.codex/skills/multidex-privacy/SKILL.md).

- Never make `analytics_enabled` or `feedback_enabled` configurable. The
  `analytics` / `feedback` tables stay parseable for schema compatibility only;
  they are inert and must not re-enable anything.
- `otel.metrics_exporter` is the sole allowed sink and only because the
  operator must name an explicit collector. It must never default to a vendor
  endpoint; the `statsig` default stays gated off by `analytics_enabled`.
- Every launch of a child process (shell, exec server, MCP server, hook, PTY,
  integration) must force `PRIVACY_ENV_VARS`. Add a newly discovered tool
  opt-out to that table rather than at the call site.
- Do not add a default-on exporter, analytics SDK, session replay, or crash
  upload. Do not add a "consent" path that turns one on when configuration is
  missing; missing configuration means off.
- Do not let a probe, a migration, or a daemon start-up path emit telemetry
  that the interactive session would not.
- Changing this behavior requires an explicit user request that names the
  destination and the data collected.

## Artifacts after a merge

After each PR merges into ModelCloud/MultiDex `main`, publish installable packages for Linux
x86_64, Linux ARM64, and macOS ARM64 from the same merged commit. Follow
[.codex/skills/multidex-release/SKILL.md](.codex/skills/multidex-release/SKILL.md).
For Linux x86_64, publish both Ubuntu 24.04 and 26.04 assets. The 24.04 build
must be compiled against glibc 2.39 or older and both assets must include or
statically link a compatible OpenSSL runtime.
The workspace version can remain unchanged across merges, so check the artifact's source
commit as well as its version.

Before substantial Rust builds, follow
[.codex/skills/multidex-build-acceleration/SKILL.md](.codex/skills/multidex-build-acceleration/SKILL.md).

When building from an Apple Silicon Mac (or without dedicated Linux builders), follow
[.codex/skills/multidex-build-from-mac/SKILL.md](.codex/skills/multidex-build-from-mac/SKILL.md).
It pins Rust to the channel declared in `codex-rs/rust-toolchain.toml`, requires the same compiler
on macOS and both Linux architectures, and covers the amd64 Ubuntu 24.04/26.04 and arm64 builds.

That skill is also the canonical reference for the `smolvm` VM commands used to build every
non-native target, including the flag set that breaks builds when omitted (`--net-backend
virtio-net`, an explicit `--mem`, `--net`, and `--oci-platform` with `--rosetta` for amd64).
Before publishing, it requires installing every asset from the published release and confirming the
reported SHA matches the source commit: a release missing
`multidex-package-x86_64-unknown-linux-gnu-ubuntu24.04.tar.gz` silently makes
`install-multidex.sh` on Ubuntu 24.04 fall back to an older release.
