#!/bin/sh

set -eu

REPOSITORY="ModelCloud/MultiDex"
os="$(uname -s)"
machine="$(uname -m)"
case "$machine" in
  x86_64|amd64) machine="x86_64" ;;
  aarch64|arm64) machine="aarch64" ;;
  *)
    echo "Unsupported MultiDex architecture: $machine" >&2
    exit 1
    ;;
esac
case "$os" in
  Linux) TARGET="$machine-unknown-linux-gnu" ;;
  Darwin) TARGET="$machine-apple-darwin" ;;
  *)
    echo "Unsupported MultiDex operating system: $os" >&2
    exit 1
    ;;
esac
RELEASE="${MULTIDEX_RELEASE:-latest}"
CODEX_HOME_DIR="${CODEX_HOME:-$HOME/.codex}"
STANDALONE_ROOT="$CODEX_HOME_DIR/packages/standalone"
RELEASES_DIR="$STANDALONE_ROOT/releases"
CURRENT_LINK="$STANDALONE_ROOT/current"
BIN_DIR="${MULTIDEX_INSTALL_DIR:-$HOME/.local/bin}"
LOCAL_ARCHIVE="${MULTIDEX_ARCHIVE:-}"
TMP_DIR=""
PREFERRED_ASSET=""
if [ "$TARGET" = "x86_64-unknown-linux-gnu" ] && [ -f /etc/os-release ]; then
  # Ubuntu releases have different glibc baselines. Choose an OS-specific
  # asset when available instead of assuming the generic binary is compatible.
  . /etc/os-release
  case "${ID:-}:${VERSION_ID:-}" in
    ubuntu:22.04|ubuntu:24.04|ubuntu:26.04)
      PREFERRED_ASSET="multidex-package-$TARGET-ubuntu$VERSION_ID.tar.gz"
      ;;
  esac
fi

cleanup() {
  if [ -n "$TMP_DIR" ]; then
    rm -rf "$TMP_DIR"
  fi
}
trap cleanup EXIT HUP INT TERM

usage() {
  cat <<'EOF'
Usage: install-multidex.sh [--release VERSION|TAG]

Installs the ModelCloud MultiDex Linux or macOS distribution as the `codex`
command. Existing CODEX_HOME configuration, auth, and session data are kept.

Environment:
  MULTIDEX_RELEASE      Release version or release tag, or latest (default).
                        `latest` selects the newest published release that
                        provides a package for this architecture, including
                        commit-specific `multidex-build-<version>-<sha>` tags.
  CODEX_HOME            Codex data home (default: ~/.codex).
  MULTIDEX_INSTALL_DIR  Directory for codex, multidex, and helper links.
  MULTIDEX_ARCHIVE      Local package archive, for offline installs/tests.
EOF
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --release)
      [ "$#" -ge 2 ] || { echo "--release requires a version" >&2; exit 2; }
      RELEASE="$2"
      shift 2
      ;;
    --help|-h)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

download() {
  url="$1"
  output="$2"
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$output"
  elif command -v wget >/dev/null 2>&1; then
    wget -q -O "$output" "$url"
  else
    echo "curl or wget is required to download MultiDex" >&2
    return 1
  fi
}

sha256_file() {
  file="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$file" | awk '{print $1}'
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$file" | awk '{print $1}'
  else
    echo "sha256sum or shasum is required to verify the MultiDex archive" >&2
    return 1
  fi
}

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/multidex-install.XXXXXX")"
ARCHIVE="$TMP_DIR/multidex.tar.gz"
CHECKSUM="$TMP_DIR/multidex.tar.gz.sha256"

if [ -n "$LOCAL_ARCHIVE" ]; then
  cp "$LOCAL_ARCHIVE" "$ARCHIVE"
  if [ -f "$LOCAL_ARCHIVE.sha256" ]; then
    cp "$LOCAL_ARCHIVE.sha256" "$CHECKSUM"
  fi
else
  asset="multidex-package-$TARGET.tar.gz"
  case "$RELEASE" in
    latest)
      # Published packages are commit-specific tags such as
      # multidex-build-<version>-<sha>, so the version tags cannot identify the
      # newest usable package. Walk the published releases instead, newest
      # first, and take the first one that ships this target. GitHub returns
      # the releases as indented JSON with "tag_name" and "prerelease" ahead of
      # the asset list for each entry.
      download "https://api.github.com/repos/$REPOSITORY/releases?per_page=50" "$TMP_DIR/releases.json"
      if [ -n "$PREFERRED_ASSET" ]; then
        RELEASE="$(awk -v asset="\"$PREFERRED_ASSET\"" '
          /"tag_name"[[:space:]]*:/ {
            line = $0
            sub(/.*"tag_name"[[:space:]]*:[[:space:]]*"/, "", line)
            sub(/".*/, "", line)
            tag = line
            prerelease = ""
          }
          /"prerelease"[[:space:]]*:/ && index($0, "true") > 0 { prerelease = "true" }
          tag ~ /^multidex-/ && prerelease != "true" && index($0, asset) > 0 { print tag; exit }
        ' "$TMP_DIR/releases.json")"
        [ -z "$RELEASE" ] || asset="$PREFERRED_ASSET"
      fi
      if [ -z "$RELEASE" ] || [ "$RELEASE" = "latest" ]; then
        RELEASE="$(awk -v asset="\"$asset\"" '
        /"tag_name"[[:space:]]*:/ {
          line = $0
          sub(/.*"tag_name"[[:space:]]*:[[:space:]]*"/, "", line)
          sub(/".*/, "", line)
          tag = line
          prerelease = ""
        }
        /"prerelease"[[:space:]]*:/ && index($0, "true") > 0 { prerelease = "true" }
        tag ~ /^multidex-/ && prerelease != "true" && index($0, asset) > 0 { print tag; exit }
        ' "$TMP_DIR/releases.json")"
      fi
      [ -n "$RELEASE" ] || {
        echo "No published MultiDex release provides $asset" >&2
        exit 1
      }
      ;;
    *)
      case "$RELEASE" in
        multidex-*) ;;
        *) RELEASE="multidex-v$RELEASE" ;;
      esac
      printf '%s\n' "$RELEASE" | grep -Eq '^multidex-[0-9A-Za-z][0-9A-Za-z.-]*$' || {
        echo "Requested MultiDex release version is invalid" >&2
        exit 1
      }
      ;;
  esac
  release_url="https://github.com/$REPOSITORY/releases/download/$RELEASE"
  if [ -n "$PREFERRED_ASSET" ] && [ "$asset" != "$PREFERRED_ASSET" ]; then
    if download "$release_url/$PREFERRED_ASSET" "$ARCHIVE" \
      && download "$release_url/$PREFERRED_ASSET.sha256" "$CHECKSUM"; then
      asset="$PREFERRED_ASSET"
    fi
  fi
  if [ "$asset" != "$PREFERRED_ASSET" ] || [ ! -s "$CHECKSUM" ]; then
    download "$release_url/$asset" "$ARCHIVE"
    download "$release_url/$asset.sha256" "$CHECKSUM"
  fi
fi

if [ -f "$CHECKSUM" ]; then
  expected="$(awk 'NR == 1 {print $1}' "$CHECKSUM")"
  actual="$(sha256_file "$ARCHIVE")"
  [ -n "$expected" ] && [ "$expected" = "$actual" ] || {
    echo "MultiDex archive checksum verification failed" >&2
    exit 1
  }
elif [ -z "$LOCAL_ARCHIVE" ]; then
  echo "MultiDex release checksum is missing" >&2
  exit 1
fi

PAYLOAD="$TMP_DIR/package"
mkdir -p "$PAYLOAD"
tar -xzf "$ARCHIVE" -C "$PAYLOAD"
MANIFEST="$PAYLOAD/codex-package.json"
[ -f "$MANIFEST" ] || { echo "MultiDex package manifest is missing" >&2; exit 1; }
version="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST")"
variant="$(sed -n 's/.*"variant"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST")"
entrypoint="$(sed -n 's/.*"entrypoint"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST")"
target="$(sed -n 's/.*"target"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$MANIFEST")"
[ -n "$version" ] && [ "$target" = "$TARGET" ] && [ "$variant" = "multidex" ] && [ "$entrypoint" = "bin/multidex" ] || {
  echo "Archive is not a supported MultiDex package" >&2
  exit 1
}
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([.-][0-9A-Za-z.-]+)?$' || {
  echo "MultiDex package version is invalid" >&2
  exit 1
}
[ -x "$PAYLOAD/bin/multidex" ] || { echo "MultiDex executable is missing" >&2; exit 1; }
[ -e "$PAYLOAD/bin/codex" ] || ln -s multidex "$PAYLOAD/bin/codex"
[ -x "$PAYLOAD/bin/codex" ] || { echo "Codex-compatible MultiDex command is missing" >&2; exit 1; }
[ -x "$PAYLOAD/bin/codex-code-mode-host" ] || {
  echo "MultiDex code-mode companion is missing" >&2
  exit 1
}

mkdir -p "$RELEASES_DIR" "$BIN_DIR"
release_dir="$RELEASES_DIR/$version-multidex-$TARGET"
staged_release="$RELEASES_DIR/.multidex-staging.$$"
rm -rf "$staged_release"
mv "$PAYLOAD" "$staged_release"
if [ -e "$release_dir" ] || [ -L "$release_dir" ]; then
  backup_release="$release_dir.previous.$(date +%s)"
  mv "$release_dir" "$backup_release"
fi
mv "$staged_release" "$release_dir"

temporary_link="$STANDALONE_ROOT/.current-multidex.$$"
ln -s "$release_dir" "$temporary_link"
# `mv -T` is GNU-only; remove the destination symlink first so this works with
# both GNU and BSD mv (an existing symlink-to-directory must be replaced, not
# followed into).
if [ -L "$CURRENT_LINK" ] || [ ! -e "$CURRENT_LINK" ]; then
  rm -f "$CURRENT_LINK"
fi
mv -f "$temporary_link" "$CURRENT_LINK"

backup_existing_link() {
  path="$1"
  if [ -e "$path" ] || [ -L "$path" ]; then
    if [ "$(readlink "$path" 2>/dev/null || true)" = "$CURRENT_LINK/bin/codex" ] \
      || [ "$(readlink "$path" 2>/dev/null || true)" = "$CURRENT_LINK/bin/multidex" ] \
      || [ "$(readlink "$path" 2>/dev/null || true)" = "$CURRENT_LINK/bin/codex-code-mode-host" ]; then
      rm -f "$path"
      return
    fi
    backup="$path.pre-multidex.$(date +%s).$$"
    mv "$path" "$backup"
    printf 'Preserved previous command at %s\n' "$backup"
  fi
}

backup_existing_link "$BIN_DIR/codex"
backup_existing_link "$BIN_DIR/multidex"
backup_existing_link "$BIN_DIR/codex-code-mode-host"
ln -s "$CURRENT_LINK/bin/codex" "$BIN_DIR/codex"
ln -s "$CURRENT_LINK/bin/multidex" "$BIN_DIR/multidex"
ln -s "$CURRENT_LINK/bin/codex-code-mode-host" "$BIN_DIR/codex-code-mode-host"

if [ "$BIN_DIR" = "$HOME/.local/bin" ]; then
  case "$(basename "${SHELL:-sh}")" in
    zsh) profile="${HOME}/.zprofile" ;;
    *) profile="${HOME}/.profile" ;;
  esac
  marker="# MultiDex CLI"
  if ! grep -Fq "$marker" "$profile" 2>/dev/null; then
    {
      printf '\n%s\n' "$marker"
      printf 'export PATH="%s:$PATH"\n' "$BIN_DIR"
    } >> "$profile"
  fi
fi

printf 'Installed MultiDex %s at %s\n' "$version" "$release_dir"
printf 'The codex command now resolves to %s\n' "$BIN_DIR/codex"
printf 'Existing Codex auth, configuration, and session data were left in %s\n' "$CODEX_HOME_DIR"
