# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Small opt-in Bazel regression; never uses the product output base or SDK.

Run with uv run, --source pointing at hermetic-llvm v0.8.11 directory.bzl,
and --scratch pointing at an existing build-artifact directory. The fixture and
logs are retained for review. Requires Bazel 9 and a local C compiler; it does
not download LLVM or Apple's SDK, or compile the product.
"""

import argparse
import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

SOURCE_SHA256 = "59a1e298a7025d636310c5a7becd420d7c185c345a073ddff78e2fcbe19e16ff"
REPO = Path(__file__).resolve().parents[2]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    parser.add_argument("--bazel", default="bazel")
    args = parser.parse_args()
    source = args.source.read_bytes()
    if hashlib.sha256(source).hexdigest() != SOURCE_SHA256:
        raise ValueError("expected the pinned hermetic-llvm v0.8.11 directory.bzl")
    root = Path(tempfile.mkdtemp(prefix="sdk-metadata-", dir=args.scratch.resolve()))
    print(f"Retained fixture: {root}", flush=True)
    workspace = root / "workspace"
    workspace.mkdir()
    (workspace / "directory.bzl").write_bytes(source)
    subprocess.run(
        [
            "patch",
            "-p1",
            "-i",
            str(REPO / "patches/llvm_reject_sdk_finder_metadata.patch"),
        ],
        cwd=workspace,
        check=True,
        timeout=10,
    )
    (workspace / "MODULE.bazel").write_text(
        'module(name = "sdk_metadata_probe")\n'
        'bazel_dep(name = "bazel_skylib", version = "1.9.0")\n'
    )
    (workspace / "BUILD.bazel").write_text('exports_files(["directory.bzl"])\n')
    compiler = shutil.which("clang")
    if sys.platform == "darwin":
        compiler = subprocess.check_output(
            ["xcrun", "--find", "clang"], text=True
        ).strip()
    if not compiler:
        raise RuntimeError("clang is required for the native-action regression")
    (workspace / "probe.bzl").write_text("""
def _probe(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".o")
    ctx.actions.run_shell(
        inputs = depset([ctx.file.src], transitive = [ctx.attr.sdk[DefaultInfo].files]),
        outputs = [out],
        arguments = [ctx.executable.compiler.path, ctx.file.src.path, out.path],
        tools = [ctx.executable.compiler],
        command = '"$1" -x c -c "$2" -o "$3" -I sdk',
        mnemonic = "SdkMetadataProbe",
    )
    return [DefaultInfo(files = depset([out]))]
probe = rule(implementation = _probe, attrs = {
    "src": attr.label(allow_single_file = True),
    "sdk": attr.label(),
    "compiler": attr.label(executable = True, cfg = "exec", allow_single_file = True),
})
""")
    sdk = workspace / "sdk"
    sdk.mkdir()
    (sdk / "BUILD.bazel").write_text(
        'load("//:directory.bzl", "headers_directory")\n'
        'headers_directory(name = "sdk", path = ".", visibility = ["//visibility:public"])\n'
    )
    (sdk / "nested").mkdir()
    header = sdk / "nested/header.h"
    header.write_text("#define SDK_VALUE 1\n")
    (sdk / "header.h").symlink_to("nested/header.h")
    (workspace / "compiler").symlink_to(compiler)
    (workspace / "probe.c").write_text('#include "header.h"\nint value = SDK_VALUE;\n')
    with (workspace / "BUILD.bazel").open("a") as stream:
        stream.write(
            'load(":probe.bzl", "probe")\n'
            'probe(name = "probe", src = "probe.c", sdk = "//sdk", compiler = "compiler")\n'
        )
    env = {**os.environ, "USE_BAZEL_VERSION": "9.0.0"}
    records = []

    def build(name: str, reject: bool = False) -> bytes:
        started = time.monotonic()
        command = [
            args.bazel,
            "--batch",
            "--ignore_all_rc_files",
            f"--output_user_root={root / 'output'}",
            "--host_jvm_args=-Xmx512m",
            "build",
            "//:probe",
            "--jobs=1",
            "--local_resources=cpu=1",
            "--spawn_strategy=sandboxed",
            "--noreuse_sandbox_directories",
            "--lockfile_mode=update",
            f"--execution_log_json_file={root / (name + '.json')}",
        ]
        result = subprocess.run(
            command,
            cwd=workspace,
            env=env,
            capture_output=True,
            text=True,
            timeout=90,
            check=False,
        )
        (root / (name + ".log")).write_text(result.stdout + result.stderr)
        records.append(
            {
                "name": name,
                "returncode": result.returncode,
                "seconds": round(time.monotonic() - started, 3),
                "argv": command,
            }
        )
        (root / "results.json").write_text(json.dumps(records, indent=2))
        if reject:
            assert result.returncode != 0, name
            assert "SDK directory contains Finder metadata" in result.stderr, (
                result.stderr
            )
            log = root / (name + ".json")
            assert not log.exists() or "SdkMetadataProbe" not in log.read_text(), name
            return b""
        assert result.returncode == 0, result.stderr
        return (workspace / "bazel-bin/probe.o").read_bytes()

    original = build("clean")
    clean_action = json.loads((root / "clean.json").read_text())
    assert clean_action["mnemonic"] == "SdkMetadataProbe"
    clean_inputs = {entry["path"]: entry["digest"] for entry in clean_action["inputs"]}
    assert "sdk/header.h" in clean_inputs and "sdk/nested/header.h" in clean_inputs
    assert build("repeat") == original
    assert not (root / "repeat.json").read_text().strip(), "repeat executed an action"
    for relative in [".DS_Store", "nested/.DS_Store"]:
        metadata = sdk / relative
        metadata.write_text("Finder metadata fixture")
        build("reject-" + relative.replace("/", "-"), reject=True)
        assert metadata.read_text() == "Finder metadata fixture"
        metadata.unlink()
    assert build("recovered") == original
    assert not (root / "recovered.json").read_text().strip(), (
        "recovery executed an action"
    )
    header.write_text("#define SDK_VALUE 2\n")
    assert build("header-edit") != original
    edited_action = json.loads((root / "header-edit.json").read_text())
    edited_inputs = {
        entry["path"]: entry["digest"] for entry in edited_action["inputs"]
    }
    assert clean_inputs["sdk/header.h"] != edited_inputs["sdk/header.h"]
    assert (sdk / "header.h").is_symlink()
    print(
        "PASS: clean/repeat/recovery, root/nested metadata rejected before action, real header edit invalidates; SDK symlink preserved"
    )


if __name__ == "__main__":
    main()
