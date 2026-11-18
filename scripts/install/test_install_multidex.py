#!/usr/bin/env python3

import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


INSTALLER = Path(__file__).with_name("install-multidex.sh")
INSTALLER_MACHINE = subprocess.check_output(["uname", "-m"], text=True).strip()
INSTALLER_ARCH = {"amd64": "x86_64", "arm64": "aarch64"}.get(
    INSTALLER_MACHINE, INSTALLER_MACHINE
)
INSTALLER_OS = subprocess.check_output(["uname", "-s"], text=True).strip()
INSTALLER_SUFFIX = "apple-darwin" if INSTALLER_OS == "Darwin" else "unknown-linux-gnu"
INSTALLER_TARGET = f"{INSTALLER_ARCH}-{INSTALLER_SUFFIX}"


class InstallerTestCase(unittest.TestCase):
    pass


def make_archive(root: Path, version: str, target: str = INSTALLER_TARGET) -> Path:
    package = root / f"package-{version}"
    (package / "bin").mkdir(parents=True)
    (package / "codex-path").mkdir()
    (package / "codex-resources").mkdir()
    manifest = {
        "layoutVersion": 1,
        "version": version,
        "target": target,
        "variant": "multidex",
        "entrypoint": "bin/multidex",
        "resourcesDir": "codex-resources",
        "pathDir": "codex-path",
    }
    (package / "codex-package.json").write_text(json.dumps(manifest), encoding="utf-8")
    for name, label in (
        ("multidex", "multidex"),
        ("codex-code-mode-host", "code-mode-host"),
    ):
        executable = package / "bin" / name
        executable.write_text(
            f"#!/bin/sh\nprintf '%s %s\\n' '{label}' \"$*\"\n", encoding="utf-8"
        )
        executable.chmod(0o755)
    (package / "codex-path" / "rg").write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
    archive = root / f"multidex-{version}.tar.gz"
    with tarfile.open(archive, "w:gz") as output:
        for path in package.rglob("*"):
            if path.is_file():
                output.add(path, arcname=path.relative_to(package))
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    Path(f"{archive}.sha256").write_text(
        f"{digest}  {archive.name}\n", encoding="utf-8"
    )
    return archive


class MultiDexInstallerTest(InstallerTestCase):
    def test_installer_does_not_require_gnu_mv(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = make_archive(root, "0.155.1")
            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            mock_mv = fake_bin / "mv"
            mock_mv.write_text(
                "#!/bin/sh\n"
                'for arg in "$@"; do\n'
                '  case "$arg" in\n'
                "    -T|-Tf|-fT)\n"
                "      echo 'mock mv: GNU-only -T option' >&2\n"
                "      exit 64\n"
                "      ;;\n"
                "  esac\n"
                "done\n"
                'exec /bin/mv "$@"\n',
                encoding="utf-8",
            )
            mock_mv.chmod(0o755)
            codex_home = root / "codex-home"
            standalone = codex_home / "packages" / "standalone"
            previous = standalone / "releases" / "previous"
            previous.mkdir(parents=True)
            (standalone / "current").symlink_to(previous)

            result = subprocess.run(
                [str(INSTALLER)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "HOME": str(root),
                    "CODEX_HOME": str(codex_home),
                    "MULTIDEX_INSTALL_DIR": str(root / "bin"),
                    "MULTIDEX_ARCHIVE": str(archive),
                    "PATH": f"{fake_bin}:{os.environ['PATH']}",
                },
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                (standalone / "current").resolve(),
                (
                    standalone / "releases" / f"0.155.1-multidex-{INSTALLER_TARGET}"
                ).resolve(),
            )

    def test_darwin_arm64_selects_apple_package(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            target = "aarch64-apple-darwin"
            asset = f"multidex-package-{target}.tar.gz"
            archive = make_archive(root, "0.155.5", target)
            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            urls = root / "requested-urls"

            mock_uname = fake_bin / "uname"
            mock_uname.write_text(
                "#!/bin/sh\n"
                'case "$1" in\n'
                "  -s) echo Darwin ;;\n"
                "  -m) echo arm64 ;;\n"
                '  *) exec /usr/bin/uname "$@" ;;\n'
                "esac\n",
                encoding="utf-8",
            )
            mock_uname.chmod(0o755)

            releases = json.dumps(
                [
                    {
                        "tag_name": "multidex-build-0.155.5-darwin",
                        "prerelease": False,
                        "assets": [
                            {"name": asset},
                            {
                                "name": (
                                    "multidex-package-aarch64-unknown-linux-gnu.tar.gz"
                                )
                            },
                        ],
                    }
                ],
                indent=2,
            )
            mock_curl = fake_bin / "curl"
            mock_curl.write_text(
                "#!/bin/sh\n"
                'url="$2"\noutput="$4"\n'
                f"printf '%s\\n' \"$url\" >> '{urls}'\n"
                'case "$url" in\n'
                "  *'/releases?per_page=50') cat > \"$output\" <<'RELEASES'\n"
                f"{releases}\n"
                "RELEASES\n"
                "    ;;\n"
                f"  *'/multidex-build-0.155.5-darwin/{asset}.sha256') cp "
                f"'{archive}.sha256' \"$output\" ;;\n"
                f"  *'/multidex-build-0.155.5-darwin/{asset}') cp "
                f"'{archive}' \"$output\" ;;\n"
                "  *) exit 9 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            mock_curl.chmod(0o755)

            result = subprocess.run(
                [str(INSTALLER)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "HOME": str(root),
                    "CODEX_HOME": str(root / "codex-home"),
                    "MULTIDEX_INSTALL_DIR": str(root / "bin"),
                    "PATH": f"{fake_bin}:{os.environ['PATH']}",
                },
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            requested = urls.read_text(encoding="utf-8")
            self.assertIn(
                "/multidex-build-0.155.5-darwin/"
                "multidex-package-aarch64-apple-darwin.tar.gz",
                requested,
            )
            self.assertNotIn("aarch64-unknown-linux-gnu", requested)

    def test_install_uses_multidex_codex_and_preserves_codex_home(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            home = root / "home"
            codex_home = root / "existing-codex-home"
            bin_dir = root / "bin"
            home.mkdir()
            bin_dir.mkdir()
            (codex_home / "sessions").mkdir(parents=True)
            (codex_home / "config.toml").write_text(
                'model = "gpt-6-sol"\n', encoding="utf-8"
            )
            (codex_home / "auth.json").write_text(
                '{"auth":"preserve-me"}\n', encoding="utf-8"
            )
            (codex_home / "sessions" / "rollout.jsonl").write_text(
                "history\n", encoding="utf-8"
            )
            official_release = (
                codex_home
                / "packages"
                / "standalone"
                / "releases"
                / f"0.154.0-codex-{INSTALLER_TARGET}"
            )
            official_codex = official_release / "bin" / "codex"
            official_codex.parent.mkdir(parents=True)
            official_codex.write_text(
                "#!/bin/sh\nprintf 'official codex %s\\n' \"$*\"\n", encoding="utf-8"
            )
            official_codex.chmod(0o755)
            current = codex_home / "packages" / "standalone" / "current"
            current.parent.mkdir(parents=True, exist_ok=True)
            current.symlink_to(official_release)
            (bin_dir / "codex").symlink_to(current / "bin" / "codex")
            previous_helper = bin_dir / "codex-code-mode-host"
            previous_helper.write_text("existing helper\n", encoding="utf-8")
            previous_helper.chmod(0o755)
            archive = make_archive(root, "0.155.1")

            result = subprocess.run(
                [str(INSTALLER)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "HOME": str(home),
                    "CODEX_HOME": str(codex_home),
                    "MULTIDEX_INSTALL_DIR": str(bin_dir),
                    "MULTIDEX_ARCHIVE": str(archive),
                    "PATH": f"{bin_dir}:{os.environ['PATH']}",
                },
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(
                (codex_home / "config.toml").read_text(), 'model = "gpt-6-sol"\n'
            )
            self.assertEqual(
                (codex_home / "auth.json").read_text(), '{"auth":"preserve-me"}\n'
            )
            self.assertEqual(
                (codex_home / "sessions" / "rollout.jsonl").read_text(), "history\n"
            )
            self.assertEqual(
                os.readlink(bin_dir / "codex"), str(current / "bin" / "codex")
            )
            self.assertEqual(
                os.readlink(bin_dir / "multidex"), str(current / "bin" / "multidex")
            )
            self.assertEqual(
                os.readlink(bin_dir / "codex-code-mode-host"),
                str(current / "bin" / "codex-code-mode-host"),
            )
            resolved = subprocess.run(
                ["codex", "--version"],
                check=True,
                capture_output=True,
                text=True,
                env={"PATH": f"{bin_dir}:{os.environ['PATH']}"},
            )
            self.assertEqual(resolved.stdout.strip(), "multidex --version")
            self.assertTrue(official_release.is_dir())
            current.unlink()
            current.symlink_to(official_release)
            rolled_back = subprocess.run(
                ["codex", "--version"],
                check=True,
                capture_output=True,
                text=True,
                env={"PATH": f"{bin_dir}:{os.environ['PATH']}"},
            )
            self.assertEqual(rolled_back.stdout.strip(), "official codex --version")
            self.assertTrue(list(bin_dir.glob("codex-code-mode-host.pre-multidex.*")))

            second_archive = make_archive(root, "0.155.3")
            updated = subprocess.run(
                [str(INSTALLER)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "HOME": str(home),
                    "CODEX_HOME": str(codex_home),
                    "MULTIDEX_INSTALL_DIR": str(bin_dir),
                    "MULTIDEX_ARCHIVE": str(second_archive),
                },
            )
            self.assertEqual(updated.returncode, 0, updated.stderr)
            self.assertEqual(
                os.readlink(current),
                str(
                    codex_home
                    / "packages"
                    / "standalone"
                    / "releases"
                    / f"0.155.3-multidex-{INSTALLER_TARGET}"
                ),
            )
            self.assertTrue(
                (
                    codex_home
                    / "packages"
                    / "standalone"
                    / "releases"
                    / f"0.155.1-multidex-{INSTALLER_TARGET}"
                ).is_dir()
            )

    def test_installer_rejects_unsafe_package_version(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = make_archive(root, "1.2.3;bad")
            result = subprocess.run(
                [str(INSTALLER)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "HOME": str(root),
                    "CODEX_HOME": str(root / "codex-home"),
                    "MULTIDEX_INSTALL_DIR": str(root / "bin"),
                    "MULTIDEX_ARCHIVE": str(archive),
                },
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("version is invalid", result.stderr)

    def test_latest_install_skips_upstream_prerelease_and_other_targets(
        self,
    ) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = make_archive(root, "0.155.4")
            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            urls = root / "requested-urls"
            asset = f"multidex-package-{INSTALLER_TARGET}.tar.gz"

            def release(tag: str, prerelease: bool, assets: list) -> dict:
                return {
                    "tag_name": tag,
                    "prerelease": prerelease,
                    "assets": [
                        {
                            "name": name,
                            "browser_download_url": (
                                "https://github.com/ModelCloud/MultiDex/"
                                f"releases/download/{tag}/{name}"
                            ),
                        }
                        for name in assets
                    ],
                }

            releases = json.dumps(
                [
                    release(
                        "rust-v9.9.9",
                        False,
                        [f"codex-package-{INSTALLER_TARGET}.tar.gz"],
                    ),
                    release(
                        "multidex-build-0.155.6-darwin-only",
                        False,
                        ["multidex-package-aarch64-apple-darwin.tar.gz"],
                    ),
                    release(
                        "multidex-build-0.155.5-beta",
                        True,
                        [asset],
                    ),
                    release(
                        "multidex-build-0.154.9",
                        False,
                        [asset],
                    ),
                ],
                indent=2,
            )

            mock_curl = fake_bin / "curl"
            mock_curl.write_text(
                "#!/bin/sh\n"
                'url="$2"\noutput="$4"\n'
                f"printf '%s\\n' \"$url\" >> '{urls}'\n"
                'case "$url" in\n'
                "  *'/releases?per_page=50') cat > \"$output\" <<'RELEASES'\n"
                f"{releases}\n"
                "RELEASES\n"
                "    ;;\n"
                f"  *'multidex-build-0.154.9/{asset}.sha256') cp "
                f"'{archive}.sha256' \"$output\" ;;\n"
                f"  *'multidex-build-0.154.9/{asset}') cp "
                f"'{archive}' \"$output\" ;;\n"
                "  *) exit 9 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            mock_curl.chmod(0o755)

            result = subprocess.run(
                [str(INSTALLER)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "HOME": str(root),
                    "CODEX_HOME": str(root / "codex-home"),
                    "MULTIDEX_INSTALL_DIR": str(root / "bin"),
                    "PATH": f"{fake_bin}:{os.environ['PATH']}",
                },
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            requested = urls.read_text(encoding="utf-8")
            self.assertIn("/releases?per_page=50", requested)
            self.assertIn("/releases/download/multidex-build-0.154.9/", requested)
            self.assertNotIn("rust-v9.9.9", requested)
            self.assertNotIn("multidex-build-0.155.6-darwin-only", requested)
            self.assertNotIn("multidex-build-0.155.5-beta", requested)

    def test_release_argument_accepts_version_and_release_tag(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = make_archive(root, "0.155.4")
            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            urls = root / "requested-urls"
            mock_curl = fake_bin / "curl"
            mock_curl.write_text(
                "#!/bin/sh\n"
                'url="$2"\noutput="$4"\n'
                f"printf '%s\\n' \"$url\" >> '{urls}'\n"
                'case "$url" in\n'
                "  *'.tar.gz.sha256') cp "
                f"'{archive}.sha256' \"$output\" ;;\n"
                "  *'.tar.gz') cp "
                f"'{archive}' \"$output\" ;;\n"
                "  *) exit 9 ;;\n"
                "esac\n",
                encoding="utf-8",
            )
            mock_curl.chmod(0o755)

            def install(release: str, home: str) -> subprocess.CompletedProcess:
                return subprocess.run(
                    [str(INSTALLER), "--release", release],
                    check=False,
                    capture_output=True,
                    text=True,
                    env={
                        **os.environ,
                        "HOME": str(root / home),
                        "CODEX_HOME": str(root / home / "codex-home"),
                        "MULTIDEX_INSTALL_DIR": str(root / home / "bin"),
                        "PATH": f"{fake_bin}:{os.environ['PATH']}",
                    },
                )

            tagged = install("multidex-build-0.157.1-18dbfada9", "tagged")
            self.assertEqual(tagged.returncode, 0, tagged.stderr)
            versioned = install("0.155.4", "versioned")
            self.assertEqual(versioned.returncode, 0, versioned.stderr)

            requested = urls.read_text(encoding="utf-8")
            self.assertIn(
                "/releases/download/multidex-build-0.157.1-18dbfada9/", requested
            )
            self.assertIn("/releases/download/multidex-v0.155.4/", requested)
            self.assertNotIn("?per_page=50", requested)

    def test_install_accepts_local_archive_without_endpoint_access(self) -> None:
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            archive = make_archive(root, "0.155.2")
            codex_home = root / "codex-home"
            bin_dir = root / "bin"
            result = subprocess.run(
                [str(INSTALLER)],
                check=False,
                capture_output=True,
                text=True,
                env={
                    **os.environ,
                    "HOME": str(root),
                    "CODEX_HOME": str(codex_home),
                    "MULTIDEX_INSTALL_DIR": str(bin_dir),
                    "MULTIDEX_ARCHIVE": str(archive),
                },
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue(
                (codex_home / "packages" / "standalone" / "current").is_symlink()
            )


if __name__ == "__main__":
    unittest.main()
