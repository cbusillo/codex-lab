# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Opt-in regression for cache-owned LLVM BUILD overlays.

Provide --source pointing at hermetic-llvm 0.8.11 and an existing --scratch
artifact directory. Default mode checks the pinned upstream functions on a
real filesystem without launching Bazel. --bazel PATH additionally exercises
Bazel 9's repository cache with two private output bases, without compiling or
downloading LLVM. Fixtures and logs are retained for review.
"""

import argparse
import ast
import hashlib
import json
import os
import shutil
import stat
import subprocess
import tempfile
import time
from pathlib import Path
from types import SimpleNamespace
from typing import Any

REPO = Path(__file__).resolve().parents[2]
SOURCE_HASHES = {
    "http_bsdtar_archive.bzl": "c0e000b2e90a51de2d67a5a3e5659caac3bb60e0a212b7420e430baef28cee81",
    "extensions/llvm_source.bzl": "a8cec0a09546d95edd39a9527144fc908c71ca85fa0ba297184f9f6011ae2204",
}


def selected_source(source: str, names: set[str]) -> str:
    """Keep exact upstream function text; omit unrelated Starlark initialization."""
    tree = ast.parse(source)
    lines = source.splitlines(keepends=True)
    return "\n".join(
        "".join(lines[node.lineno - 1 : node.end_lineno])
        for node in tree.body
        if isinstance(node, ast.FunctionDef) and node.name in names
    )


def load_functions(source: Path) -> dict[str, Any]:
    text = (source / "extensions/llvm_source.bzl").read_text()
    tree = ast.parse(text)
    constants = [
        node
        for node in tree.body
        if isinstance(node, ast.Assign)
        and any(
            isinstance(t, ast.Name)
            and t.id
            in {"_LLVM_PROJECT_OVERLAY_FILES", "_LLVM_SOURCE_BSDTAR_EXTRA_ARGS"}
            for t in node.targets
        )
    ]
    scope: dict[str, Any] = {
        "Label": lambda label: label,
        "structs": SimpleNamespace(to_dict=lambda value: dict(vars(value))),
        "fail": lambda message: (_ for _ in ()).throw(ValueError(message)),
    }
    exec(
        compile(
            ast.Module(body=constants, type_ignores=[]), "overlay-constants", "exec"
        ),
        scope,
    )
    exec(
        selected_source(
            text,
            {
                "_create_llvm_raw_repo",
                "_llvm_project_overlay_files",
                "_llvm_source_archive_excludes",
            },
        ),
        scope,
    )
    helper = selected_source(
        (source / "http_bsdtar_archive.bzl").read_text(), {"symlink_files"}
    )
    exec(helper, scope)
    return scope


class FileContext:
    """Filesystem adapter for the selected upstream function, not a Bazel emulator."""

    def __init__(
        self,
        root: Path,
        files: dict[str, str],
        copies: list[str],
        labels: dict[str, Path],
    ):
        self.root = root
        self.labels = labels
        self.attr = SimpleNamespace(files=files, copy_build_files=copies)
        self.watched: set[Path] = set()

    def path(self, value: str) -> SimpleNamespace:
        target = self.labels.get(value, self.root / value)
        return SimpleNamespace(value=target, exists=target.exists())

    def watch(self, source: SimpleNamespace) -> None:
        self.watched.add(source.value)

    def delete(self, value: str) -> None:
        (self.root / value).unlink()

    def symlink(self, source: SimpleNamespace, destination: str) -> None:
        target = self.root / destination
        target.parent.mkdir(parents=True, exist_ok=True)
        target.symlink_to(source.value)

    def template(
        self, destination: str, source: SimpleNamespace, *, executable: bool
    ) -> None:
        assert not executable, "BUILD overlays should not become executable"
        target = self.root / destination
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(source.value.read_bytes())


def offline(source: Path, root: Path, *, patched: bool) -> dict[str, Any]:
    scope = load_functions(source)
    defaults = scope["_LLVM_PROJECT_OVERLAY_FILES"]
    calls: list[dict[str, Any]] = []
    scope["http_bsdtar_archive"] = lambda **kwargs: calls.append(kwargs)
    scope["new_local_repository"] = lambda **kwargs: calls.append(
        {"kind": "local", **kwargs}
    )
    scope["git_repository"] = lambda **kwargs: calls.append({"kind": "git", **kwargs})
    config = SimpleNamespace(source_archive=SimpleNamespace(files=defaults))
    scope["_create_llvm_raw_repo"](SimpleNamespace(modules=[]), config)
    default_call = calls.pop()
    provider = root / "A" / "provider"
    provider.mkdir(parents=True)
    labels = {}
    for index, label in enumerate(defaults.values()):
        path = provider / f"overlay-{index}"
        path.write_text(f'filegroup(name = "original_{index}")\n')
        labels[label] = path
    cache = root / "cache"
    cache.mkdir()
    ctx = FileContext(
        cache,
        default_call["files"],
        list(default_call.get("copy_build_files", [])),
        labels,
    )
    scope["symlink_files"](ctx)
    assert ctx.watched == set(labels.values())
    # Changing an input and re-evaluating must replace the old materialization.
    for path in labels.values():
        path.write_text('filegroup(name = "changed")\n')
    scope["symlink_files"](ctx)
    b = root / "B"
    b.symlink_to(cache, target_is_directory=True)
    shutil.rmtree(root / "A")
    surviving = [name for name in defaults if (b / name).exists()]
    assert len(surviving) == (len(defaults) if patched else 0), surviving
    if patched:
        for name in defaults:
            assert not (b / name).is_symlink()
            assert '"changed"' in (b / name).read_text()
    # Explicit overrides, including non-text payloads, keep the files contract.
    overridden = next(iter(defaults))
    custom = {overridden: "//:custom", "extra.bin": "//:binary"}
    tag = SimpleNamespace(files=custom)
    tags = SimpleNamespace(from_archive=[tag], from_path=[], from_git=[])
    scope["_create_llvm_raw_repo"](
        SimpleNamespace(modules=[SimpleNamespace(tags=tags)]), config
    )
    override_call = calls.pop()
    assert override_call["files"] == {**defaults, **custom}
    if patched:
        assert set(override_call["copy_build_files"]) == set(defaults) - {overridden}
    custom_root = root / "custom-inputs"
    custom_root.mkdir()
    for index, label in enumerate(override_call["files"].values()):
        path = custom_root / str(index)
        path.write_bytes(
            b"\x00\xff\x80" if label == "//:binary" else b"# custom overlay\n"
        )
        path.chmod(0o755)
        labels[label] = path
    custom_cache = root / "custom-cache"
    custom_cache.mkdir()
    scope["symlink_files"](
        FileContext(
            custom_cache,
            override_call["files"],
            list(override_call.get("copy_build_files", [])),
            labels,
        )
    )
    for name, label in custom.items():
        result = custom_cache / name
        assert result.is_symlink() and result.resolve() == labels[label]
        assert result.read_bytes() == labels[label].read_bytes()
        assert result.stat().st_mode & stat.S_IXUSR
    for kind, value in [
        ("from_path", SimpleNamespace(path="/fixture")),
        ("from_git", SimpleNamespace(remote="fixture")),
    ]:
        tags = SimpleNamespace(from_archive=[], from_path=[], from_git=[])
        setattr(tags, kind, [value])
        scope["_create_llvm_raw_repo"](
            SimpleNamespace(modules=[SimpleNamespace(tags=tags)]), config
        )
        call = calls.pop()
        assert "copy_build_files" not in call
    return {
        "mode": "offline",
        "patched": patched,
        "surviving_overlays": len(surviving),
        "overlays": len(defaults),
    }


def remove_owned_output(output: Path, identity: tuple[int, int]) -> None:
    current = output.lstat()
    assert (current.st_dev, current.st_ino) == identity and not output.is_symlink()
    # Batch-mode commands have exited. Never follow external cache symlinks.
    for directory, _, _ in os.walk(output, followlinks=False):
        info = os.lstat(directory)
        assert info.st_uid == os.getuid() and info.st_dev == current.st_dev
        os.chmod(directory, stat.S_IMODE(info.st_mode) | 0o700, follow_symlinks=False)
    shutil.rmtree(output)


def bazel_fixture(
    source: Path, root: Path, bazel: str, *, patched: bool
) -> dict[str, Any]:
    scope = load_functions(source)
    overlays = list(scope["_LLVM_PROJECT_OVERLAY_FILES"])
    helper = selected_source(
        (source / "http_bsdtar_archive.bzl").read_text(), {"symlink_files"}
    )
    root.mkdir()
    workspace = root / "workspace"
    workspace.mkdir()
    (workspace / "MODULE.bazel").write_text(
        'module(name="overlay_probe")\nprobe = use_extension("//:fixture.bzl", "probe")\nuse_repo(probe, "llvm-raw")\n'
    )
    (workspace / "BUILD.bazel").write_text('exports_files(["fixture.bzl"])\n')
    fixture = """
OVERLAYS = %s
%s

def _provider(ctx):
    version = ctx.getenv("OVERLAY_VERSION", "before")
    for index in range(len(OVERLAYS)):
        ctx.file("overlay-%%s.bazel" %% index, 'filegroup(name="selected", srcs=["%%s"])' %% version, executable=False)
    ctx.file("BUILD.bazel", 'exports_files(glob(["overlay-*.bazel"]))')
    return ctx.repo_metadata(reproducible=False)

provider = repository_rule(implementation=_provider)

def _consumer(ctx):
    for name in OVERLAYS:
        directory = name.rsplit("/", 1)[0]
        ctx.file(directory + "/before", "old")
        ctx.file(directory + "/after", "new")
    symlink_files(ctx)
    ctx.file("BUILD.bazel", "# fixture")
    return ctx.repo_metadata(reproducible=True)

consumer = repository_rule(implementation=_consumer, attrs={
    "files": attr.string_keyed_label_dict(),
    "copy_build_files": attr.string_list(),
})

def _extension(ctx):
    provider(name="provider")
    consumer(name="llvm-raw", files={name: "@provider//:overlay-%%s.bazel" %% index for index, name in enumerate(OVERLAYS)}, copy_build_files=%s)

probe = module_extension(implementation=_extension)
""" % (repr(overlays), helper, "OVERLAYS" if patched else "[]")
    (workspace / "fixture.bzl").write_text(fixture)
    cache = root / "repo-cache"
    records = []
    expression = (
        "labels(srcs, set("
        + " ".join("@llvm-raw//" + p.rsplit("/", 1)[0] + ":selected" for p in overlays)
        + "))"
    )

    def query(
        output: Path, phase: str, version: str = "before"
    ) -> subprocess.CompletedProcess[str]:
        started = time.monotonic()
        result = subprocess.run(
            [
                bazel,
                "--batch",
                "--ignore_all_rc_files",
                "--noexperimental_remote_repo_contents_cache",
                f"--output_user_root={root / 'user'}",
                f"--output_base={output}",
                "--host_jvm_args=-Xmx512m",
                "query",
                expression,
                f"--repo_contents_cache={cache}",
                f"--repository_cache={root / 'downloads'}",
                f"--repo_env=OVERLAY_VERSION={version}",
                "--lockfile_mode=update",
            ],
            cwd=workspace,
            capture_output=True,
            text=True,
            timeout=90,
            check=False,
        )
        (root / f"{phase}.log").write_text(result.stdout + result.stderr)
        records.append(
            {
                "phase": phase,
                "exit": result.returncode,
                "seconds": round(time.monotonic() - started, 3),
            }
        )
        (root / "results.json").write_text(json.dumps(records, indent=2))
        return result

    a, b = root / "A", root / "B"
    for initial_output in (a, b):
        initial_result = query(initial_output, initial_output.name)
        assert initial_result.returncode == 0, initial_result.stderr
        assert initial_result.stdout.count(":before") == len(overlays), (
            initial_result.stdout
        )
    targets = [
        next(
            (
                p
                for p in (output / "external").iterdir()
                if p.name.endswith("+llvm-raw")
            ),
            None,
        )
        for output in (a, b)
    ]
    assert all(targets), "missing fixture repository"
    resolved = [target.resolve() for target in targets if target is not None]
    assert resolved[0] == resolved[1] and resolved[0].is_relative_to(cache), (
        "fixture did not exercise shared cache reuse"
    )
    cached_overlays = [resolved[0] / name for name in overlays]
    for overlay in cached_overlays:
        assert overlay.exists()
        assert overlay.is_symlink() is not patched
        if not patched:
            assert Path(os.readlink(overlay)).is_relative_to(a), (
                "negative control must depend on A"
            )
    identity = a.stat()
    remove_owned_output(a, (identity.st_dev, identity.st_ino))
    assert all(overlay.exists() is patched for overlay in cached_overlays)
    after = query(b, "after-A-removal")
    if patched:
        assert after.returncode == 0 and after.stdout.count(":before") == len(
            overlays
        ), after.stderr
        edited = query(b, "changed-input", "after")
        assert edited.returncode == 0 and edited.stdout.count(":after") == len(
            overlays
        ), edited.stderr
    else:
        assert after.returncode != 0, (
            "negative control did not reproduce the dangling overlay failure"
        )
        assert "llvm-raw" in after.stderr and any(
            message in after.stderr.lower()
            for message in ("no such package", "build file", "no such file")
        ), after.stderr
    return {"mode": "bazel", "patched": patched, "records": records}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    parser.add_argument("--bazel", help="Opt in to the real Bazel 9 cache fixture")
    args = parser.parse_args()
    root = Path(tempfile.mkdtemp(prefix="llvm-overlay-", dir=args.scratch.resolve()))
    print(f"Retained fixture: {root}", flush=True)
    original, patched = root / "original", root / "patched"
    for name, expected in SOURCE_HASHES.items():
        content = (args.source / name).read_bytes()
        if hashlib.sha256(content).hexdigest() != expected:
            raise ValueError(f"unexpected pinned source: {name}")
        for destination in (original, patched):
            target = destination / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(content)
    subprocess.run(
        [
            "patch",
            "-F0",
            "-p1",
            "-i",
            str(REPO / "patches/llvm_cache_owned_overlays.patch"),
        ],
        cwd=patched,
        check=True,
        timeout=10,
    )
    results = []
    for source, fixed in [(original, False), (patched, True)]:
        results.append(offline(source, root / f"offline-{fixed}", patched=fixed))
        if args.bazel:
            results.append(
                bazel_fixture(
                    source, root / f"bazel-{fixed}", args.bazel, patched=fixed
                )
            )
    (root / "summary.json").write_text(json.dumps(results, indent=2))
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
