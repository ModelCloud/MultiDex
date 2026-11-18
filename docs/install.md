## Installing & building

### System requirements

| Requirement                 | Details                                                                  |
| --------------------------- | ------------------------------------------------------------------------ |
| Operating systems           | Linux x86_64/ARM64 (Ubuntu 20.04+/Debian 10+ or compatible), macOS ARM64 |
| Git (optional, recommended) | 2.23+ for built-in PR helpers                                            |
| RAM                         | 4-GB minimum (8-GB recommended)                                          |

### Install the MultiDex release

```bash
curl -fsSL https://github.com/ModelCloud/MultiDex/releases/latest/download/install-multidex.sh | sh
```

The installer adds the MultiDex binary as `codex`, installs
`codex-code-mode-host`, and keeps existing Codex configuration, auth, and
session history under `CODEX_HOME`. It does not contact configured inference
endpoints. Releases are versioned; for rollback, point
`$CODEX_HOME/packages/standalone/current` at a prior directory under
`$CODEX_HOME/packages/standalone/releases/`.

### Build from source

```bash
# Clone the repository and navigate to the root of the Cargo workspace.
git clone https://github.com/ModelCloud/MultiDex.git
cd MultiDex/codex-rs

# Install the Rust toolchain, if necessary.
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source "$HOME/.cargo/env"
rustup component add rustfmt
rustup component add clippy
# Install helper tools used by the workspace justfile:
cargo install --locked just
# DotSlash fetches pinned development tools such as buildifier on first use.
cargo install --locked dotslash
# Install nextest for the `just test` helper.
cargo install --locked cargo-nextest

# Build MultiDex and its Code Mode companion.
cargo build --bin codex --bin codex-code-mode-host

# Launch the TUI with a sample prompt.
cargo run --bin codex -- "explain this codebase to me"

# After making changes, use the root justfile helpers (they default to codex-rs):
just fmt
just fix -p <crate-you-touched>

# Run the relevant tests (project-specific is fastest), for example:
just test -p codex-tui
# `just test` runs the test suite via nextest:
just test
# Avoid `--all-features` for routine local runs because it increases build
# time and `target/` disk usage by compiling additional feature combinations.
```

## Tracing / verbose logging

Codex is written in Rust, so it honors the `RUST_LOG` environment variable to configure its logging behavior.

The TUI records diagnostics in bounded local stores by default. Set `log_dir` explicitly to enable a plaintext TUI log for a run:

```bash
codex -c log_dir=./.codex-log
tail -F ./.codex-log/codex-tui.log
```

The non-interactive mode (`codex exec`) defaults to `RUST_LOG=error`, but messages are printed inline, so there is no need to monitor a separate file.

See the Rust documentation on [`RUST_LOG`](https://docs.rs/env_logger/latest/env_logger/#enabling-logging) for more information on the configuration options.
