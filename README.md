<p align="center"><strong>MultiDex</strong> is a Codex-compatible coding agent that runs locally on your computer.
<p align="center">
  <img src="https://github.com/openai/codex/blob/main/.github/codex-cli-splash.png" alt="Codex CLI splash" width="80%" />
</p>
</br>
If you want Codex in your code editor (VS Code, Cursor, Windsurf), <a href="https://developers.openai.com/codex/ide">install in your IDE.</a>
</br>If you want the desktop app experience, run <code>codex app</code> or visit <a href="https://chatgpt.com/codex?app-landing-page=true">the Codex App page</a>.
</br>If you are looking for the <em>cloud-based agent</em> from OpenAI, <strong>Codex Web</strong>, go to <a href="https://chatgpt.com/codex">chatgpt.com/codex</a>.</p>

---

## Experimental host-managed worker messaging

The app-server exposes an opt-in BGR v1 bridge for host-managed brain/grunt
threads and isolated reviewers: identity-fenced tasks, clarification/progress/result callbacks,
correlated status self-summaries, safe-boundary replies, and live
attempt inspection/cancellation. Trusted host delivery supports external brains
without a proxy native brain thread. Cancellation fences queued BGR queries as
well as late replies, without dropping unrelated mailbox/input. Omnigent remains
the scheduler and durable ledger. Atomic reviewer creation freezes exact Git
base/head source and exposes only snapshot/BGR tools, with no shell, MCP,
delegation, ambient instructions, or arbitrary filesystem access. Hosts must require
`reviewerSnapshotNoLazyFetch: true` before launching reviewers: snapshot capture
disables Git lazy fetching and denies all transports; missing promised objects fail closed. Restart
recovery remains unsupported. See the
[implemented contract and capability limits](docs/brain-executor-reviewer-design.md#implemented-backend-contract-version-1).

## Quickstart

### Installing and running MultiDex

Linux x86_64 and ARM64 (including NVIDIA GB10 Spark), plus macOS ARM64,
releases are supported. The installer selects the package for the current
platform and architecture, installs the `codex` command and its Code Mode
companion, and leaves the existing `CODEX_HOME` auth, configuration, and
session files in place:

```shell
curl -fsSL https://github.com/ModelCloud/MultiDex/releases/latest/download/install-multidex.sh | sh
```

To pin a release, pass `--release VERSION` to the downloaded installer. Each
upgrade keeps prior versioned binaries under `CODEX_HOME/packages/standalone/`
so rollback can switch the `current` symlink back to the previous release.
Local model endpoints are not contacted during installation or upgrade.

To roll back, inspect the retained release directories and repoint `current`:

```shell
CODEX_HOME_DIR="${CODEX_HOME:-$HOME/.codex}"
ls "$CODEX_HOME_DIR/packages/standalone/releases"
ln -sfn "$CODEX_HOME_DIR/packages/standalone/releases/<previous-release>" \
  "$CODEX_HOME_DIR/packages/standalone/current"
```

### Configure named OpenAI-compatible providers

OpenRouter is built in: set `experimental_bearer_token` under
`[model_providers.openrouter]` in `CODEX_HOME/config.toml` and select
`deepseek/deepseek-v4.1-flash-1017`. See [OpenRouter configuration](docs/config.md#multidex-providers)
for Wafer-only routing. Both the local and OpenRouter DS v4.1 Flash entries
default to Max reasoning and use the DeepSeek Harness prompt adapted to MultiDex's tools.

MultiDex keeps the built-in `openai` provider and its normal ChatGPT/API-key
authentication. Add custom providers by name in `CODEX_HOME/config.toml`; each
provider has its own endpoint and bearer-token environment variable:

```toml
model = "gpt-6-sol"
model_provider = "openai"

[model_providers.dsv41]
name = "DSV4.1 endpoint"
base_url = "http://10.0.13.33:2120/v1"
env_key = "DSV41_BEARER_TOKEN"
wire_api = "responses"
requires_openai_auth = false

[profiles.dsv41]
model = "DS v4.1 Flash 1017"
model_provider = "dsv41"
```

Set `DSV41_BEARER_TOKEN` in the environment before launching MultiDex, then
select the `dsv41` profile with `codex --profile dsv41`. The bearer value stays
out of TOML, command-line arguments, and MultiDex logs. Repeat the provider and
profile sections with distinct names for additional endpoints. Do not set a
global `openai_base_url` when you need official OpenAI models and custom models
to share the same session.

For Omnigent, put provider definitions in
`~/.local/share/multidex/config.toml` and model-to-provider mappings in
`~/.local/share/multidex/models.toml`. This keeps the registrations out of
Omnigent's global provider catalog while letting one MultiDex harness offer
official and custom models together:

```toml
# ~/.local/share/multidex/config.toml
[model_providers.dsv41]
name = "DSV4.1 endpoint"
base_url = "http://10.0.13.33:2120/v1"
env_key = "DSV41_BEARER_TOKEN"
wire_api = "responses"
requires_openai_auth = false

[model_providers.lab]
name = "Lab endpoint"
base_url = "https://models.example/v1"
env_key = "LAB_BEARER_TOKEN"
wire_api = "responses"
requires_openai_auth = false
```

```toml
# ~/.local/share/multidex/models.toml
[models."DS v4.1 Flash 1017"]
provider = "dsv41"
display_name = "DS v4.1 Flash 1017"
discover_capabilities = true

[models."lab/coder-32b"]
provider = "lab"
display_name = "Lab Coder 32B"
```

Export the named bearer variables in the Omnigent host service environment.
Only models with their configured bearer variable present are offered as
routable; selecting one routes to its named provider. Official Codex models
continue to use the normal ChatGPT login and OpenAI endpoints. Installation and
upgrade never contact or validate a model endpoint.

An orchestrator attaching a TUI with `--remote` can set
`MULTIDEX_SKIP_RESUME_GOAL_PROMPT=1` to keep the composer available when resuming
a paused, blocked, or usage-limited goal. This only hides the resume-choice
modal; it does not resume the goal. Interactive remote connections still prompt
by default, and local sessions ignore this flag.

If an endpoint returns `response.incomplete` with reason `max_output_tokens`,
it has reached its output budget. MultiDex reports that reason immediately and
stops the request without retrying or switching compaction models. Increase the
inference server's output-token limit or request a shorter response before
trying again; increasing connection retries will not raise the output budget.

### Switching models and providers

In `/model`, DeepSeek v4.1 Flash appears once. Select its provider (Local,
OpenRouter, or LithosAI), then a variant when available (LithosAI Regular,
Fast, or Ultra), and finally the reasoning effort. Models available from only
one provider stay in the flat list.

The model picker routes the bundled OpenAI, MultiDex, OpenRouter, and Lithos
models to their providers without discarding conversation history. The selected
model's context budget applies to the next turn. Moving to a smaller window can
require compaction before sampling; moving to a larger window does not by itself
require compaction. A model's compaction-compatibility hash can independently
require a fresh summary.

After a cold resume, an endpoint transition without verified previous-provider
state uses the current model/provider for required compaction instead of sending
the previous endpoint's model ID to the new provider.

To check routing, credentials, retained history, and runtime context changes with
fake keys and loopback endpoints:

```sh
python3 scripts/test_lithos_provider_handoff.py --binary /path/to/multidex
```

### Model at capacity (`server_is_overloaded`)

When the provider reports the selected model is at capacity ("Selected model is
at capacity. Please try a different model."), MultiDex waits and resumes the
request automatically — no user interaction and no retry budget is consumed;
the request retries indefinitely until capacity frees up. A `Retry-After` hint
from the provider is honored; otherwise the delay defaults to 1 second and is
configurable per provider or globally:

```toml
[model_providers.dsv41]
# ... other fields ...
server_overloaded_retry_delay_ms = 2000
```

```bash
# Global override (also covers the built-in openai provider)
export CODEX_SERVER_OVERLOADED_RETRY_DELAY_MS=2000
```

<details>
<summary>You can also go to the <a href="https://github.com/openai/codex/releases/latest">latest GitHub Release</a> and download the appropriate binary for your platform.</summary>

Each GitHub Release contains many executables, but in practice, you likely want one of these:

- macOS
  - Apple Silicon/arm64: `codex-aarch64-apple-darwin.tar.gz`
  - x86_64 (older Mac hardware): `codex-x86_64-apple-darwin.tar.gz`
- Linux
  - x86_64: `codex-x86_64-unknown-linux-musl.tar.gz`
  - arm64: `codex-aarch64-unknown-linux-musl.tar.gz`

Each archive contains a single entry with the platform baked into the name (e.g., `codex-x86_64-unknown-linux-musl`), so you likely want to rename it to `codex` after extracting it.

</details>

### Using Codex with your ChatGPT plan

Run `codex` and select **Sign in with ChatGPT**. We recommend signing into your ChatGPT account to use Codex as part of your Plus, Pro, Business, Edu, or Enterprise plan. [Learn more about what's included in your ChatGPT plan](https://help.openai.com/en/articles/11369540-codex-in-chatgpt).

You can also use Codex with an API key, but this requires [additional setup](https://developers.openai.com/codex/auth#sign-in-with-an-api-key).

## Docs

- [**Codex Documentation**](https://developers.openai.com/codex)
- [**Contributing**](./docs/contributing.md)
- [**Installing & building**](./docs/install.md)
- [**Open source fund**](./docs/open-source-fund.md)

This repository is licensed under the [Apache-2.0 License](LICENSE).
