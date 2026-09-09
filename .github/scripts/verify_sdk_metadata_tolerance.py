# /// script
# requires-python = ">=3.12"
# dependencies = []
# ///
"""Opt-in native regression for SDK inputs and LLVM module maps.

Pass --source the pinned hermetic-llvm v0.8.11 directory.bzl (with sibling
runtimes/module_map.bzl), --rust-utils the pinned rules_rust rust/private/utils.bzl,
and --scratch an existing build-artifact directory.
Uses an isolated Bazel 9 workspace and local clang; retains evidence and stops
its own server. No product build, shared SDK mutation or LLVM download.
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

REPO = Path(__file__).resolve().parents[2]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--rust-utils", type=Path, required=True)
    parser.add_argument("--scratch", type=Path, required=True)
    parser.add_argument("--bazel", default="bazel")
    args = parser.parse_args()
    source = args.source.read_text()
    module_source = (args.source.parent / "runtimes/module_map.bzl").read_text()
    assert (
        hashlib.sha256(source.encode()).hexdigest()
        == "59a1e298a7025d636310c5a7becd420d7c185c345a073ddff78e2fcbe19e16ff"
    )
    assert (
        hashlib.sha256(module_source.encode()).hexdigest()
        == "f40854658786420982590802cee79f0670035b97f4d2c3a8051e8b2087e7c7ca"
    )
    rust_source = args.rust_utils.read_text()
    assert (
        hashlib.sha256(rust_source.encode()).hexdigest()
        == "f499e43eb641cc27099b42e65d3c0febae0b0dfdfff8e3014d945408bc92cc4a"
    )
    root = Path(tempfile.mkdtemp(prefix="sdk-tolerance-", dir=args.scratch.resolve()))
    workspace = root / "workspace"
    workspace.mkdir()
    print(f"Evidence: {root}", flush=True)
    (workspace / "runtimes").mkdir()
    (workspace / "directory.bzl").write_text(source)
    (workspace / "runtimes/module_map.bzl").write_text(module_source)
    subprocess.run(
        ["patch", "-p1", "-i", str(REPO / "patches/llvm_sdk_metadata_inputs.patch")],
        cwd=workspace,
        check=True,
        timeout=10,
    )
    (workspace / "rust/private").mkdir(parents=True)
    rust_utils = workspace / "rust/private/utils.bzl"
    rust_utils.write_text(rust_source)
    subprocess.run(
        ["patch", "-p1", "-i", str(REPO / "patches/rules_rust_directory_paths.patch")],
        cwd=workspace,
        check=True,
        timeout=10,
    )
    patched_utils = rust_utils.read_text()
    # Load the exact patched expansion functions without unrelated Rust rules;
    # their ctx and provider operations execute under real Bazel analysis.
    expansion = patched_utils[
        patched_utils.index(
            "def _expand_location_for_build_script_runner"
        ) : patched_utils.index("def expand_list_element_locations")
    ]
    (workspace / "expansion.bzl").write_text(
        'load("@bazel_skylib//rules/directory:providers.bzl", "DirectoryInfo")\n'
        + expansion
    )
    filtered = (workspace / "directory.bzl").read_text()
    filtered_module = (workspace / "runtimes/module_map.bzl").read_text()
    variants = {"raw": source, "filtered": filtered, "subpath": filtered}
    (workspace / "MODULE.bazel").write_text(
        'module(name = "sdk_tolerance")\nbazel_dep(name = "bazel_skylib", version = "1.9.0")\nbazel_dep(name="bazel_features", version="1.42.0")\n'
    )
    compiler = (
        subprocess.check_output(["xcrun", "--find", "clang"], text=True).strip()
        if sys.platform == "darwin"
        else shutil.which("clang")
    )
    if not compiler:
        raise RuntimeError("clang is required")
    (workspace / "compiler").symlink_to(compiler)
    (workspace / "probe.c").write_text('#include "header.h"\nint value = SDK_VALUE;\n')
    (workspace / "probe.bzl").write_text("""
load("@bazel_skylib//rules/directory:providers.bzl", "DirectoryInfo")
load("//:expansion.bzl", "expand_dict_value_locations")
def _probe(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".o")
    ctx.actions.run_shell(
        inputs = depset([ctx.file.src], transitive = [ctx.attr.sdk[DefaultInfo].files]),
        outputs = [out],
        arguments = [ctx.executable.compiler.path, ctx.file.src.path, out.path, ctx.attr.sdk[DirectoryInfo].path],
        tools = [ctx.executable.compiler],
        command = '"$1" -x c -c "$2" -o "$3" -I "$4"',
        mnemonic = "SdkToleranceProbe",
    )
    return [DefaultInfo(files = depset([out]))]
def _env(ctx):
    out = ctx.actions.declare_file(ctx.label.name + ".json")
    ctx.actions.write(out, json.encode(expand_dict_value_locations(ctx, ctx.attr.env, ctx.attr.data, {})))
    return [DefaultInfo(files = depset([out]))]
env_probe = rule(implementation = _env, attrs = {
    "env": attr.string_dict(),
    "data": attr.label_list(allow_files = True),
})
def _tree(ctx):
    out = ctx.actions.declare_directory(ctx.label.name)
    ctx.actions.run_shell(outputs = [out], arguments = [out.path], command = 'mkdir -p "$1"; echo "#define TREE_HEADER 1" > "$1/tree.h"')
    return [DefaultInfo(files = depset([out]))]
tree = rule(implementation = _tree)
probe = rule(implementation = _probe, attrs = {
    "src": attr.label(allow_single_file = True),
    "sdk": attr.label(),
    "compiler": attr.label(executable = True, cfg = "exec", allow_single_file = True),
})
""")
    build = [
        'load(":probe.bzl", "probe", "env_probe", "tree")',
        'exports_files(glob(["*.bzl"]))',
        'tree(name="generated")',
    ]
    (workspace / "textual").mkdir()
    (workspace / "textual/header.h").write_text("#define TEXTUAL_HEADER 1\n")
    (workspace / "textual/BUILD.bazel").write_text(
        'load("@bazel_skylib//rules/directory:directory.bzl", "directory")\n'
        'directory(name="textual", srcs=["header.h"], visibility=["//visibility:public"])\n'
    )
    sdks = {}
    for name in variants:
        mod = module_source if name == "raw" else filtered_module
        (workspace / f"{name}_module.bzl").write_text(
            mod.replace('"//:directory.bzl"', f'"//:{name}.bzl"')
        )
        build.append(
            f'load(":{name}_module.bzl", {name}_include="include_path", {name}_map="module_map")'
        )
    for name, rule in variants.items():
        (workspace / f"{name}.bzl").write_text(rule)
        package = workspace / f"sdk_{name}"
        path = "sysroot" if name == "subpath" else "."
        sdk = package / path
        sdks[name] = sdk
        (sdk / "nested").mkdir(parents=True)
        (sdk / "nested/header.h").write_text("#define SDK_VALUE 1\n")
        (sdk / "header.h").symlink_to("nested/header.h")
        (package / "BUILD.bazel").write_text(
            f'load("//:{name}.bzl", "headers_directory")\nheaders_directory(name="sdk", path="{path}", visibility=["//visibility:public"])\n'
        )
        build.append(
            f'probe(name="{name}", src="probe.c", sdk="//sdk_{name}:sdk", compiler="compiler")'
        )
        build.append(
            f'{name}_include(name="{name}_include", srcs=["//sdk_{name}:sdk", ":generated", "//textual"])'
        )
        build.append(f'{name}_map(name="{name}_map", include_path=":{name}_include")')
        if name != "raw":
            build.append(
                f'env_probe(name="{name}_env", data=["//sdk_{name}:sdk", "probe.c"], env={{"SDK":"$(directorypath //sdk_{name}:sdk)", "FILE":"$(location :probe.c)", "ESCAPED":"$$(directorypath :missing)"}})'
            )
    (workspace / "BUILD.bazel").write_text("\n".join(build) + "\n")
    external = root / "external-sdk"
    (external / "sdk").mkdir(parents=True)
    (external / "MODULE.bazel").write_text(
        'module(name="sdk_fixture")\nbazel_dep(name="bazel_skylib", version="1.9.0")\n'
    )
    (external / "sdk/header.h").write_text("#define EXTERNAL_HEADER 1\n")
    (external / "sdk/BUILD.bazel").write_text(
        'load("@bazel_skylib//rules/directory:directory.bzl", "directory")\n'
        'directory(name="sdk", srcs=["header.h"], visibility=["//visibility:public"])\n'
    )
    with (workspace / "MODULE.bazel").open("a") as stream:
        stream.write(
            'bazel_dep(name="sdk_fixture", repo_name="sdk_alias")\n'
            f'local_path_override(module_name="sdk_fixture", path={json.dumps(str(external))})\n'
        )
    with (workspace / "BUILD.bazel").open("a") as stream:
        stream.write(
            'env_probe(name="external_env", data=["@sdk_alias//sdk"], '
            'env={"SDK":"$(directorypath @sdk_alias//sdk)"})\n'
        )
    env = {**os.environ, "USE_BAZEL_VERSION": "9.0.0"}
    startup = [
        args.bazel,
        "--ignore_all_rc_files",
        "--noexperimental_remote_repo_contents_cache",
        f"--output_user_root={root / 'user'}",
        f"--output_base={root / 'output'}",
        "--host_jvm_args=-Xmx512m",
    ]
    flags = [
        "--jobs=1",
        "--local_resources=cpu=1",
        "--spawn_strategy=sandboxed",
        "--noreuse_sandbox_directories",
        "--disk_cache=",
        "--remote_cache=",
        "--remote_executor=",
        "--lockfile_mode=update",
        f"--repository_cache={root / 'downloads'}",
        f"--repo_contents_cache={root / 'repo-cache'}",
    ]
    records = []

    def decode_log(log_path):
        text = log_path.read_text() if log_path.exists() else ""
        decoder = json.JSONDecoder()
        decoded = []
        while text.strip():
            item, end = decoder.raw_decode(text.lstrip())
            decoded.append(item)
            text = text.lstrip()[end:]
        return [item for item in decoded if item.get("mnemonic") == "SdkToleranceProbe"]

    def run(phase_name, force=False):
        if force:
            for variant_name in variants:
                (workspace / f"bazel-bin/{variant_name}.o").unlink()
        start = time.monotonic()
        log = root / f"{phase_name}.json"
        command = [
            *startup,
            "build",
            "//:all",
            *flags,
            f"--execution_log_json_file={log}",
        ]
        build_result = subprocess.run(
            command,
            cwd=workspace,
            env=env,
            capture_output=True,
            text=True,
            timeout=90,
            check=False,
        )
        (root / f"{phase_name}.log").write_text(
            build_result.stdout + build_result.stderr
        )
        assert build_result.returncode == 0, build_result.stderr
        execution_actions = decode_log(log)
        query = subprocess.run(
            [
                *startup,
                "aquery",
                'mnemonic("SdkToleranceProbe", set(//:raw //:subpath //:filtered))',
                *flags,
                "--output=jsonproto",
            ],
            cwd=workspace,
            env=env,
            capture_output=True,
            text=True,
            timeout=45,
            check=False,
        )
        assert query.returncode == 0, query.stderr
        (root / f"{phase_name}-aquery.json").write_text(query.stdout)
        graph = json.loads(query.stdout)
        labels = {t["id"]: t["label"].rsplit(":", 1)[-1] for t in graph["targets"]}
        keys = {labels[a["targetId"]]: a["actionKey"] for a in graph["actions"]}
        assert set(keys) == set(variants) and all(keys.values())
        executed = {a["targetLabel"].rsplit(":", 1)[-1] for a in execution_actions}
        assert all(not a.get("cacheHit", False) for a in execution_actions), (
            "unexpected execution cache hit"
        )
        maps = {}
        for variant_name, sdk_dir in sdks.items():
            module_map = (
                workspace / f"bazel-bin/{variant_name}_map.modulemap"
            ).read_text()
            sdk_relative = sdk_dir.relative_to(workspace).as_posix()
            assert f'umbrella "{sdk_relative}"' in module_map, module_map
            assert 'textual header "textual/header.h"' in module_map, module_map
            assert '/generated"' in module_map, module_map
            maps[variant_name] = module_map.replace(sdk_relative, "SDK")
            if variant_name != "raw":
                expanded = json.loads(
                    (workspace / f"bazel-bin/{variant_name}_env.json").read_text()
                )
                assert expanded == {
                    "SDK": "${pwd}/" + sdk_relative,
                    "FILE": "${pwd}/./probe.c",
                    "ESCAPED": "$(directorypath :missing)",
                }, expanded
        # Whitespace may differ when grouping source and generated directories.
        assert {" ".join(m.split()) for m in maps.values()} == {
            " ".join(maps["raw"].split())
        }
        object_hashes = {
            variant_name: hashlib.sha256(
                (workspace / f"bazel-bin/{variant_name}.o").read_bytes()
            ).hexdigest()
            for variant_name in variants
        }
        record = {
            "phase": phase_name,
            "seconds": round(time.monotonic() - start, 3),
            "executed": sorted(executed),
            "objects": object_hashes,
            "action_keys": keys,
        }
        records.append(record)
        (root / "results.json").write_text(json.dumps(records, indent=2))
        print(json.dumps(record), flush=True)
        return {
            a["targetLabel"].rsplit(":", 1)[-1]: a for a in execution_actions
        }, object_hashes

    def digests(action):
        return {item["path"]: item["digest"] for item in action["inputs"]}

    try:
        baseline, original_objects = run("clean")
        assert set(baseline) == set(variants)
        baseline_inputs = digests(baseline["filtered"])
        baseline_action_key = records[-1]["action_keys"]["filtered"]
        assert (
            "sdk_filtered/header.h" in baseline_inputs
            and "sdk_filtered/nested/header.h" in baseline_inputs
        )
        repeated, _ = run("warm")
        assert not repeated
        external_env = json.loads(
            (workspace / "bazel-bin/external_env.json").read_text()
        )
        assert external_env == {"SDK": "${pwd}/external/sdk_fixture+/sdk"}
        for phase, content in [
            ("metadata-add", "metadata 1"),
            ("metadata-edit", "metadata 2 changed"),
            ("metadata-remove", None),
        ]:
            for name in variants:
                sdk = sdks[name]
                for relative in [".DS_Store", "nested/.DS_Store"]:
                    p = sdk / relative
                    if content is None:
                        p.unlink()
                    else:
                        p.write_text(content)
            actions, objects = run(phase)
            assert set(actions) == {"raw"}, (phase, actions.keys())
            assert objects == original_objects
            forced, _ = run(phase + "-forced", force=True)
            assert set(forced) == set(variants)
            assert digests(forced["filtered"]) == baseline_inputs, phase
            assert forced["filtered"].get("commandArgs") == baseline["filtered"].get(
                "commandArgs"
            )
            assert forced["filtered"].get("environmentVariables") == baseline[
                "filtered"
            ].get("environmentVariables")
            assert records[-1]["action_keys"]["filtered"] == baseline_action_key, (
                "action key changed"
            )
            assert all(not p.endswith(".DS_Store") for p in digests(forced["filtered"]))
        for name in variants:
            (sdks[name] / "nested/header.h").write_text("#define SDK_VALUE 2\n")
        edited, objects = run("real-header-edit")
        assert set(edited) == set(variants)
        assert all(objects[n] != original_objects[n] for n in variants)
        assert (
            digests(edited["filtered"])["sdk_filtered/header.h"]
            != baseline_inputs["sdk_filtered/header.h"]
        )
        for name in variants:
            assert (sdks[name] / "header.h").is_symlink()
        # Fail clearly for malformed macros and targets without DirectoryInfo.
        bad = workspace / "bad"
        bad.mkdir()
        (bad / "file.txt").write_text("not a directory")
        for name, value, error in [
            ("unclosed", "$(directorypath :file.txt", "unclosed $(directorypath"),
            ("file", "$(directorypath :file.txt)", "requires a DirectoryInfo target"),
            ("absent", "$(directorypath :missing)", "requires a DirectoryInfo target"),
        ]:
            (bad / "BUILD.bazel").write_text(
                'load("//:probe.bzl", "env_probe")\n'
                f'env_probe(name="bad", data=["file.txt"], env={{"SDK":{json.dumps(value)}}})\n'
            )
            result = subprocess.run(
                [*startup, "build", "//bad", *flags],
                cwd=workspace,
                env=env,
                capture_output=True,
                text=True,
                timeout=45,
                check=False,
            )
            (root / f"invalid-{name}.log").write_text(result.stdout + result.stderr)
            assert result.returncode != 0 and error in result.stderr, result.stderr
        (root / "PASS").write_text(
            "Native SDK input and module-map regression passed. Full toolchain qualification is separate.\n"
        )
        print(
            "PASS: filtered inputs tolerate metadata mutations; raw control rebuilds; real header edits rebuild every arm; root/subpath SDKs, symlink inclusion and umbrella module maps retained.",
            flush=True,
        )
    finally:
        stop = subprocess.run(
            [*startup, "shutdown"],
            cwd=workspace,
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
            check=False,
        )
        (root / "shutdown.log").write_text(stop.stdout + stop.stderr)
        print(f"Private Bazel shutdown: {stop.returncode}", flush=True)


if __name__ == "__main__":
    main()
