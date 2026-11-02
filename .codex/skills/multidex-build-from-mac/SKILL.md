---
name: multidex-build-from-mac
description: Build and publish every MultiDex target (Linux amd64 for Ubuntu 24.04 and 26.04, Linux arm64, macOS arm64) from a single Apple Silicon Mac using smolvm VMs. Use when a release must be built without dedicated Linux builders, when a Mac is the only build host, or when codifying the smolvm/VM commands used for those builds.
---

# Build every MultiDex target from one Apple Silicon Mac

One Mac (M-series, 32 GiB+ recommended) builds all four release assets using `smolvm` — a
lightweight VM tool (libkrun, Hypervisor.framework) that needs no daemon and no Docker. It runs
genuine VMs with their own kernel, so the arm64 builds are native and the x86_64 builds run
under Rosetta 2.

Never use qemu user emulation (`docker --platform linux/amd64` on arm64). qemu mis-executes the
`aws-lc-sys` assembly and dies with `SIGSEGV` during the C/asm compile, producing either a failed
build or a subtly broken binary. Rosetta runs real x86_64 code and does not have this problem.

## The four assets

Every release must ship all of these. The names are not cosmetic — `install-multidex.sh` selects
by exact filename.

| Target | Asset filename | Built by |
|---|---|---|
| macOS arm64 | `multidex-package-aarch64-apple-darwin.tar.gz` | native on the Mac |
| Linux arm64 | `multidex-package-aarch64-unknown-linux-gnu.tar.gz` | smolvm arm64 guest |
| Linux amd64, Ubuntu 24.04 | `multidex-package-x86_64-unknown-linux-gnu-ubuntu24.04.tar.gz` | smolvm Rosetta guest, `ubuntu:24.04` |
| Linux amd64, Ubuntu 26.04 | `multidex-package-x86_64-unknown-linux-gnu-ubuntu26.04.tar.gz` | generic amd64 archive + bundled OpenSSL 3.5 |
| plain amd64 fallback | `multidex-package-x86_64-unknown-linux-gnu.tar.gz` | same build as the 24.04 asset |

Plus `.sha256` beside each archive, a combined `SHA256SUMS`, and `install-multidex.sh`.

**Publish `-ubuntu24.04` even though the generic asset is byte-identical.** The installer sets
`PREFERRED_ASSET=multidex-package-$TARGET-ubuntu$VERSION_ID.tar.gz` on Ubuntu 24.04 and 26.04. If
that asset is absent, the `latest` walker silently falls through to an *older* release that does
have it — installing stale code while reporting success. See "Verify end to end" below; this bug
shipped once already.

## Preflight (run once per Mac)

```bash
brew install e2fsprogs coreutils patchelf
export PATH="/opt/homebrew/opt/e2fsprogs/sbin:/opt/homebrew/opt/coreutils/libexec/gnubin:$PATH"
```

- `e2fsprogs` — smolvm needs `resize2fs` to create a storage disk smaller than the 20 GiB template.
  Homebrew installs it to `sbin`, which is **not** on the default `PATH`; without it every VM
  prints `WARN failed to pre-format disk on host ... resize2fs not found` and silently falls back
  to the 20 GiB template.
- `coreutils` — macOS `realpath` has no `-m`. `package_multidex_linux_amd64_ubuntu2604.sh` uses
  `realpath -m` and fails with `realpath: illegal option -- m`. The gnubin directory must precede
  `/usr/bin`.
- `patchelf` — required by the 26.04 packaging script.

Confirm smolvm is present and at a known version:

```bash
smolvm --version        # 1.19.3 at the time of writing
```

## Pin Rust to the repo's declared version

`codex-rs/rust-toolchain.toml` declares the channel. Read it, never guess:

```bash
git show <SHA>:codex-rs/rust-toolchain.toml   # channel = "1.99.1" at 721a79a74
rustc +1.99.1 --version                       # 1.99.1 (58cf8f9c3 2026-10-13)
```

macOS and both guest architectures must report the **same** rustc commit.

On macOS, `~/.local/bin/{rustc,cargo}` must be rustup shims. If they are direct symlinks into
`~/.rustup/toolchains/stable-*`, they bypass both `rust-toolchain.toml` and `RUSTUP_TOOLCHAIN` and
silently build with the wrong compiler:

```bash
ln -sf /opt/homebrew/bin/rustup ~/.local/bin/rustc
ln -sf /opt/homebrew/bin/rustup ~/.local/bin/cargo
```

A toolchain directory can also exist yet be stale. If `rustc +1.99.1` prints a different version
than the directory name implies, reinstall it:

```bash
rustup toolchain uninstall 1.99.1-aarch64-apple-darwin
rustup toolchain install  1.99.1-aarch64-apple-darwin --profile minimal
```

Build invocations should prepend the pinned toolchain's bin directory so `cargo <subcommand>`
works regardless of shim resolution:

```bash
export PATH="$HOME/.rustup/toolchains/1.99.1-aarch64-apple-darwin/bin:$PATH"
```

## smolvm rules that matter for builds

These are the flags that actually break builds when omitted.

| Rule | Why |
|---|---|
| Always pass `--net-backend virtio-net` | The default `tsi` backend stalls cargo/crates.io downloads and appears to hang. |
| Always pass `--mem` explicitly | Default is only **8192 MiB**. Rust linking OOMs and the VM exits **137**, which looks like a random compiler crash. Use `--mem 24576`+ for amd64. |
| Always pass `--net` for image pulls and cargo | Networking is off by default; pulls fail with `networking is disabled` / `lookup index.docker.io ... network is unreachable`. |
| `--gpu` implies no network | Combine `--gpu --net` or pulls fail even though `--net` feels implied. |
| Set `--guest-subnet` if the guest runs Tailscale | The default guest link is `100.96.0.0/30`, inside Tailscale's `100.64.0.0/10` CGNAT range. Use `--guest-subnet 10.200.0.0/30`. |
| `machine exec` uses `--name` | It is a flag, not a positional: `smolvm machine exec --name vm -- cmd`. |
| `--init` scripts run inside the guest | A host path will not be found; bind-mount the script (`-v /host/dir:/h:ro`) and pass `/h/script.sh`. |
| Write results to a mounted host directory | `machine run` is ephemeral and the VM is **deleted** when the workload exits. Output left in the guest is lost. |
| An empty `machine ls` after a `machine run` is success, not failure | The ephemeral VM self-cleans on exit. Judge by the artifacts on the mounted host path, not by VM state. |
| Run restarts/long builds in `tmux` | macOS has no `setsid`; a background job dies with its parent shell. |

Clone **inside** the guest. A bind-mounted host git worktree has `.git` pointing at host-absolute
paths, so `git rev-parse` fails in the guest.

## Dependencies inside every Linux guest

```bash
apt-get install -y build-essential pkg-config libssl-dev clang mold patchelf \
                   cmake python3 curl git ca-certificates xz-utils libcap-dev
```

`libcap-dev` is mandatory: `codex-bwrap`'s build script probes `libcap` via pkg-config for Linux
targets and fails hard without it. `sccache` and `mold` are picked up automatically by the build
scripts; both are skipped **silently** when absent, so their absence costs build time but does not
fail the build.

## Target 1 — macOS arm64 (native, no VM)

```bash
export CARGO_TARGET_DIR=$repo/codex-rs/target-multidex-release-darwin
export STABLE_GIT_COMMIT=<full-sha>
export CODEX_REPO_ROOT=$repo
ulimit -n 4096      # default 256 fails a parallel build
python3 scripts/build_codex_package.py \
  --target aarch64-apple-darwin --variant multidex \
  --cargo-profile multidex-release --package-version 0.164.0 \
  --package-dir "$dist/package" \
  --archive-output "$dist/multidex-0.164.0-aarch64-apple-darwin.tar.gz" --force
cp "$dist/multidex-0.164.0-aarch64-apple-darwin.tar.gz" \
   "$dist/multidex-package-aarch64-apple-darwin.tar.gz"
(cd "$dist" && shasum -a 256 multidex-package-aarch64-apple-darwin.tar.gz \
   > multidex-package-aarch64-apple-darwin.tar.gz.sha256)
```

## Target 2 — Linux arm64 (native-speed smolvm guest)

```bash
smolvm machine run --net --net-backend virtio-net \
  --image ubuntu:24.04 --cpus 12 --mem 24576 --storage 60 \
  -v /path/to/artifacts:/out \
  -- bash /out/build_arm64.sh
```

Inside the guest:

```bash
git clone https://github.com/ModelCloud/MultiDex.git /root/src
cd /root/src && git checkout <full-sha> && test "$(git rev-parse HEAD)" = <full-sha>
export PATH=/root/.cargo/bin:$PATH
export MULTIDEX_DIST_DIR=/out/arm64
scripts/build_multidex_linux_aarch64.sh
```

Capture the build output to a host file so a failure is diagnosable after the VM is gone:

```bash
... -- bash -c 'exec > >(tee /out/arm64.log) 2>&1; /out/build_arm64.sh'
```

## Target 3 — Linux amd64, Ubuntu 24.04

`machine create` has **no** platform flag. Rosetta x86_64 requires `machine run` with
`--oci-platform`, because the platform is chosen while pulling the image:

```bash
smolvm machine run --net --net-backend virtio-net --rosetta \
  --oci-platform linux/amd64 --image ubuntu:24.04 \
  --cpus 12 --mem 24576 --storage 60 \
  -v /path/to/artifacts:/out -- bash /out/build_x86.sh
```

Inside the guest:

```bash
export CARGO_TARGET_DIR=/work/codex-rs/target-multidex-release
export MULTIDEX_DIST_DIR=/out/x86/ubuntu2404
scripts/build_multidex_linux_amd64_ubuntu2404.sh
# stage the generic name too — it is the same bytes
cp /out/x86/ubuntu2404/multidex-package-x86_64-unknown-linux-gnu-ubuntu24.04.tar.gz \
   /out/x86/multidex-package-x86_64-unknown-linux-gnu.tar.gz
```

That script hard-requires Ubuntu 24.04, links OpenSSL statically, and fails if either binary needs
glibc newer than 2.39 or still depends on system `libssl`/`libcrypto`.

**Rosetta is roughly 5× slower than native** (measured: 3.35 s native vs 16.06 s emulated on an
identical CPU-bound workload). Budget accordingly and prefer many vCPUs; the build is
CPU-bound and parallelises across cores about as well as native.

## Target 4 — Linux amd64, Ubuntu 26.04

Built on the Mac from the generic amd64 archive plus a bundled OpenSSL 3.5 runtime:

```bash
scripts/build_multidex_linux_amd64.sh        # generic base archive
scripts/package_multidex_linux_amd64_ubuntu2604.sh \
  "$dist/amd64-base/multidex-0.164.0-x86_64-unknown-linux-gnu.tar.gz" \
  /path/to/openssl-3.5-libs \
  "$dist/ubuntu2604"
```

The OpenSSL directory must contain **x86-64** `libssl.so.3` and `libcrypto.so.3` — verify with
`file`, since an arm64 or `/lib`-nested path is a common mistake. The script rewrites rpath to
`$ORIGIN/../lib` and bundles both libraries under `lib/`.

Do not attempt to validate the 26.04 asset on the `ubuntu:26.04` Docker image under Rosetta: that
image ships coreutils in `/usr/lib/cargo/bin`, which Rosetta refuses
(`Security violation: Requested utility 'rosetta' does not match executable name`). Validate the
asset by forcing the installer's OS detection on a clean `ubuntu:24.04` x86_64 guest instead.

## Verify every archive before publishing

Check the manifest, both executables, the real binary architecture, and the checksum:

```bash
for f in multidex-package-*.tar.gz; do
  echo "=== $f ==="
  tar -xOzf "$f" codex-package.json | python3 -c \
    'import json,sys; d=json.load(sys.stdin); print(d["variant"], d["target"], d["version"])'
  tar -tzf "$f" | grep -E "bin/(multidex|codex-code-mode-host)$"
  mkdir -p "/tmp/verify/$f" && tar -xzf "$f" -C "/tmp/verify/$f" \
    bin/multidex bin/codex-code-mode-host
  file "/tmp/verify/$f/bin/multidex" "/tmp/verify/$f/bin/codex-code-mode-host" \
    | sed 's/, BuildID.*//'
  shasum -a 256 -c <(awk -v n="$f" '$2==n' SHA256SUMS) 2>/dev/null || \
    shasum -a 256 -c "$f.sha256"
done
```

Expected architectures: `Mach-O 64-bit executable arm64`, `ELF 64-bit ... ARM aarch64`, and
`ELF 64-bit ... x86-64`. A mismatch here means the wrong toolchain or platform was used and the
build must be discarded.

## Publish

Create a commit-specific draft, upload each asset as soon as it verifies, then flip live:

```bash
gh release create multidex-build-<version>-<shortsha> \
  --repo ModelCloud/MultiDex --draft --title "..." --notes "source: <full-sha>"

gh release upload multidex-build-<version>-<shortsha> --repo ModelCloud/MultiDex --clobber \
  multidex-package-<target>.tar.gz multidex-package-<target>.tar.gz.sha256 install-multidex.sh

gh release edit multidex-build-<version>-<shortsha> --repo ModelCloud/MultiDex --draft=false
```

`ModelCloud/MultiDex` is the canonical repo name.

Publishing a partial release is safe — `latest` resolves per-target — but a release missing
`-ubuntu24.04` causes the stale-fallback bug described above, so check all four names are present
before announcing:

```bash
gh release view <tag> --repo ModelCloud/MultiDex --json assets --jq '.assets[].name'
```

## Verify end to end, from the published release

Do not trust the build; install from GitHub and run it. This catches the stale-fallback bug and
any missing-asset mistake. All four must report the **full source SHA**:

```bash
# macOS arm64 — run directly on the Mac
curl -fsSL https://github.com/ModelCloud/MultiDex/releases/latest/download/install-multidex.sh \
  -o /tmp/inst.sh
CODEX_HOME=/tmp/ch MULTIDEX_INSTALL_DIR=/tmp/bin bash /tmp/inst.sh
/tmp/bin/multidex --version          # codex-cli 0.164.0+<full-sha>
```

```bash
# each Linux target — inside a smolvm guest
smolvm machine run --net --net-backend virtio-net \
  [--rosetta --oci-platform linux/amd64] --image ubuntu:24.04 \
  -v /tmp/e2e:/h:ro -- bash -c '
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq && apt-get install -y -qq curl ca-certificates
    bash /h/inst.sh >/dev/null 2>&1
    /root/.local/bin/multidex --version'
```

The installer defaults to `$HOME/.local/bin`, not `/usr/local/bin`, unless
`MULTIDEX_INSTALL_DIR` is set — check the path the installer prints rather than assuming.

If any target reports the wrong SHA, the release is missing that target's asset and the walker fell
back to an older release. Fix the asset, do not just re-run.

## Quick reference

| Target | Invocation | Script |
|---|---|---|
| macOS arm64 | native | `scripts/build_codex_package.py --variant multidex` |
| Linux arm64 | `machine run --net --net-backend virtio-net --image ubuntu:24.04` | `scripts/build_multidex_linux_aarch64.sh` |
| amd64 24.04 | `machine run --net --net-backend virtio-net --rosetta --oci-platform linux/amd64 --image ubuntu:24.04` | `scripts/build_multidex_linux_amd64_ubuntu2404.sh` |
| amd64 26.04 | native on Mac | `build_multidex_linux_amd64.sh` + `package_multidex_linux_amd64_ubuntu2604.sh` |

Minimum invocation shape for any smolvm build, missing none of these:

```bash
smolvm machine run -d --name <vm> \
  --net --net-backend virtio-net --image ubuntu:24.04 \
  --cpus 12 --mem 24576 --storage 60 \
  -v <hostdir>:/out -- bash /out/build.sh
```
