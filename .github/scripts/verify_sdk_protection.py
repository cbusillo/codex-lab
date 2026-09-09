# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Exercise the patched, pinned repository function with real macOS permissions.

Run with uv run and --source pointing to upstream v0.8.11 http_pkg_archive.bzl.
Package download/extraction is a fixture; protection commands run unchanged.
No network, installed SDK, or product output directory is used.
"""

import argparse
import hashlib
import os
import shutil
import subprocess
import tempfile
from pathlib import Path
from types import SimpleNamespace

SOURCE_SHA256 = "03591d76b9a63914bfcfa104c69a9af495d0da876b8b559e3f9ed0249a40ebcc"
REPO = Path(__file__).resolve().parents[2]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    args = parser.parse_args()
    source = args.source.read_bytes()
    if hashlib.sha256(source).hexdigest() != SOURCE_SHA256:
        raise ValueError("expected pinned upstream http_pkg_archive.bzl")
    if os.uname().sysname != "Darwin":
        raise RuntimeError("native permission qualification requires macOS")
    with tempfile.TemporaryDirectory(dir=args.scratch) as temp:
        root = Path(temp)
        upstream = root / "http_pkg_archive.bzl"
        upstream.write_bytes(source)
        subprocess.run(
            [
                "patch",
                "-p1",
                "-i",
                str(REPO / "patches/llvm_protect_extracted_sdk.patch"),
            ],
            cwd=root,
            check=True,
            capture_output=True,
        )
        body = upstream.read_text().split("def _http_pkg_archive_impl", 1)[1]
        body = "def _http_pkg_archive_impl" + body.split("\nhttp_pkg_archive =", 1)[0]

        def fail(message):
            raise RuntimeError(message)

        namespace = {
            "get_auth": lambda *_: {},
            "Label": str,
            "fail": fail,
            "repo_utils": SimpleNamespace(platform=lambda _: "darwin_arm64"),
        }
        exec(compile(body, str(upstream), "exec"), namespace)
        shared = root / "shared"
        shared.mkdir()
        (shared / "header").write_text("outside content")
        shared.chmod(0o555)
        try:
            for platform in ["mac os x", "linux"]:
                work = root / platform
                work.mkdir()
                sdk = work / "sdk"

                class Context:
                    os = SimpleNamespace(name=platform)
                    attr = SimpleNamespace(
                        urls=[],
                        sha256="",
                        strip_prefix="",
                        includes=[],
                        excludes=[],
                        dst="sdk",
                        files={"sdk/BUILD.bazel": "overlay"},
                    )

                    @staticmethod
                    def download(**kwargs):
                        (work / kwargs["output"]).touch()

                    @staticmethod
                    def path(value):
                        return value

                    @staticmethod
                    def read(_):
                        return "generated overlay"

                    @staticmethod
                    def file(name, text):
                        (work / name).write_text(text)

                    @staticmethod
                    def delete(name):
                        (work / name).unlink()

                    @staticmethod
                    def repo_metadata(**kwargs):
                        return kwargs

                    @staticmethod
                    def execute(argv):
                        if argv[0].endswith("//:bin/pkgutil"):
                            (sdk / "nested").mkdir(parents=True)
                            (sdk / "nested/header.h").write_text("sdk content")
                            for seed_directory in [sdk, sdk / "nested"]:
                                (seed_directory / ".DS_Store").write_text("metadata")
                            (sdk / "alias").symlink_to(
                                "nested", target_is_directory=True
                            )
                            (sdk / "outside").symlink_to(
                                shared, target_is_directory=True
                            )
                            return SimpleNamespace(return_code=0)
                        result = subprocess.run(
                            argv, cwd=work, capture_output=True, text=True, check=False
                        )
                        return SimpleNamespace(
                            return_code=result.returncode,
                            stdout=result.stdout,
                            stderr=result.stderr,
                        )

                assert namespace["_http_pkg_archive_impl"](Context()) == {
                    "reproducible": True
                }
                assert (sdk / "alias/header.h").read_text() == "sdk content"
                assert (sdk / "BUILD.bazel").read_text() == "generated overlay"
                assert shared.stat().st_mode & 0o777 == 0o555
                if platform == "mac os x":
                    assert not list(sdk.rglob(".DS_Store"))
                    for metadata_parent in [sdk, sdk / "nested"]:
                        try:
                            (metadata_parent / ".DS_Store").write_text("unexpected")
                        except PermissionError:
                            pass
                        else:
                            raise AssertionError("protected SDK accepted metadata")
                else:
                    assert (sdk / ".DS_Store").exists()
                for directory, _, _ in os.walk(sdk, followlinks=False):
                    Path(directory).chmod(0o755)
                shutil.rmtree(sdk)
        finally:
            shared.chmod(0o755)
    print(
        "PASS: native protection, metadata removal, aliases, shared target, non-mac scope"
    )


if __name__ == "__main__":
    main()
