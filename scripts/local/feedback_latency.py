#!/usr/bin/env python3
"""Measure a named development-feedback lane and emit public-safe evidence."""

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import subprocess
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from build_storage import MAX_PATH_COUNT, parse_path_spec
from feedback_context import build_context
from feedback_storage import StorageSampler


REPO_ROOT = Path(__file__).resolve().parents[2]
SCHEMA_VERSION = 3
MAX_JSON_BYTES = 16 * 1024
MAX_SUMMARY_BYTES = 4 * 1024
MACHINE_ID_SALT = "codex-lab-feedback-latency-v1"
SCCACHE_IDENTITY_SALT = "codex-lab-sccache-identity-v1"
HARNESS_EXIT_CODE = 125
RUSTY_V8_ENV_VARS = ("RUSTY_V8_ARCHIVE", "RUSTY_V8_SRC_BINDING_PATH")
SAFE_LANE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]{0,119}\Z")
SHA256_HEX = re.compile(r"[0-9a-fA-F]{64}\Z")
EMPTY_DIFF_SHA256 = hashlib.sha256(b"").hexdigest()
SCCACHE_GAUGES = {"cacheSizeBytes", "maxCacheSizeBytes"}


class FeedbackLatencyError(Exception):
    """Raised when trustworthy evidence cannot be produced."""


def run_text(command: list[str], description: str, cwd: Path) -> str:
    try:
        result = subprocess.run(
            command,
            cwd=cwd,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except OSError as error:
        raise FeedbackLatencyError(f"could not run {description}: {error}") from error
    if result.returncode != 0:
        detail = (
            result.stderr.strip() or result.stdout.strip() or "no diagnostic output"
        )
        raise FeedbackLatencyError(
            f"{description} failed (exit {result.returncode}): {detail}"
        )
    return result.stdout.strip()


def source_identity(repo_root: Path) -> dict[str, Any]:
    commit = run_text(
        ["git", "rev-parse", "--verify", "HEAD"], "Git commit lookup", repo_root
    ).lower()
    if len(commit) != 40 or any(
        character not in "0123456789abcdef" for character in commit
    ):
        raise FeedbackLatencyError("Git did not report an exact 40-character commit")
    status = run_text(
        ["git", "status", "--porcelain", "--untracked-files=all"],
        "Git status lookup",
        repo_root,
    )
    return {
        "commit": commit,
        "dirty": bool(status),
        "untrackedChanges": any(line.startswith("?? ") for line in status.splitlines()),
        "trackedDiffSha256": tracked_diff_sha256(repo_root),
    }


def tracked_diff_sha256(repo_root: Path) -> str:
    """Hash the complete tracked worktree diff without retaining diff text."""

    try:
        process = subprocess.Popen(
            [
                "git",
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--binary",
                "HEAD",
                "--",
            ],
            cwd=repo_root,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
    except OSError as error:
        raise FeedbackLatencyError("could not run Git diff lookup") from error
    if process.stdout is None:
        process.kill()
        process.wait()
        raise FeedbackLatencyError("Git diff lookup did not provide output")
    digest = hashlib.sha256()
    try:
        for chunk in iter(lambda: process.stdout.read(1024 * 1024), b""):
            digest.update(chunk)
    finally:
        process.stdout.close()
    if process.wait() != 0:
        raise FeedbackLatencyError("Git diff lookup failed")
    return digest.hexdigest()


def source_status(snapshot: dict[str, Any], expected_diff_sha256: str | None) -> str:
    if snapshot.get("status") == "unavailable":
        return "unavailable"
    if expected_diff_sha256 is None:
        return "clean" if not snapshot.get("dirty", True) else "unknown-dirty"
    if snapshot.get("untrackedChanges"):
        return "untracked-changes"
    if snapshot.get("trackedDiffSha256") == EMPTY_DIFF_SHA256:
        return "empty-edit"
    if snapshot.get("trackedDiffSha256") != expected_diff_sha256:
        return "diff-mismatch"
    return "matched"


def source_edit_evidence(
    before: dict[str, Any],
    after: dict[str, Any],
    expected_diff_sha256: str | None,
) -> dict[str, Any]:
    before_status = source_status(before, expected_diff_sha256)
    after_status = source_status(after, expected_diff_sha256)
    stable = (
        before.get("commit") == after.get("commit")
        and before.get("trackedDiffSha256") == after.get("trackedDiffSha256")
        and before.get("dirty") == after.get("dirty")
        and before.get("untrackedChanges") == after.get("untrackedChanges")
        and before_status == after_status
    )
    comparable = (
        before_status == "matched" and after_status == "matched" and stable
        if expected_diff_sha256 is not None
        else before_status == "clean" and after_status == "clean" and stable
    )
    return {
        "mode": "controlled-edit" if expected_diff_sha256 else "uncontrolled",
        "expectedDiffSha256": expected_diff_sha256,
        "beforeStatus": before_status,
        "afterStatus": after_status,
        "stable": stable,
        "comparable": comparable,
    }


def parse_sha256(value: str) -> str:
    if SHA256_HEX.fullmatch(value) is None:
        raise argparse.ArgumentTypeError("expected a 64-character SHA-256 hex digest")
    return value.lower()


def machine_identity(environment: dict[str, str] | None = None) -> dict[str, Any]:
    if environment is None:
        environment = os.environ
    runner_environment = environment.get("RUNNER_ENVIRONMENT")
    runner_kind = (
        runner_environment
        if runner_environment in {"github-hosted", "self-hosted"}
        else "local"
    )
    operating_system = environment.get("RUNNER_OS") or platform.system() or "unknown"
    architecture = environment.get("RUNNER_ARCH") or platform.machine() or "unknown"
    cpu_count = os.cpu_count()
    cpu_label = "unknown" if cpu_count is None else str(cpu_count)
    machine_id = "local-machine"
    if runner_kind == "github-hosted":
        private_identity = "|".join(
            (
                environment.get("RUNNER_NAME") or "unknown-runner",
                operating_system,
                architecture,
                cpu_label,
            )
        )
        digest = hashlib.sha256(
            f"{MACHINE_ID_SALT}|{private_identity}".encode("utf-8")
        ).hexdigest()[:12]
        machine_id = f"machine-{digest}"
    identity: dict[str, Any] = {
        "kind": runner_kind,
        "os": operating_system,
        "arch": architecture,
        "cpuCount": cpu_count,
        "machineId": machine_id,
    }
    image = environment.get("ImageOS")
    if runner_kind == "github-hosted" and image:
        identity["image"] = image[:64]
    return identity


def integer_total(value: object) -> int | None:
    if isinstance(value, bool):
        return None
    if isinstance(value, int):
        return value
    if isinstance(value, dict):
        if "counts" in value:
            return integer_total(value["counts"])
        values = [integer_total(item) for item in value.values()]
        integers = [item for item in values if item is not None]
        return sum(integers) if integers else 0
    if isinstance(value, list):
        values = [integer_total(item) for item in value]
        integers = [item for item in values if item is not None]
        return sum(integers) if integers else 0
    return None


def parse_sccache_stats(output: str) -> dict[str, int | None]:
    try:
        payload = json.loads(output)
    except json.JSONDecodeError as error:
        raise FeedbackLatencyError(f"sccache emitted invalid JSON: {error}") from error
    stats = payload.get("stats") if isinstance(payload, dict) else None
    if not isinstance(stats, dict):
        raise FeedbackLatencyError("sccache JSON did not contain a stats object")
    fields = {
        "compileRequests": "compile_requests",
        "requestsExecuted": "requests_executed",
        "cacheHits": "cache_hits",
        "cacheMisses": "cache_misses",
        "cacheWrites": "cache_writes",
        "cacheWriteErrors": "cache_write_errors",
        "cacheErrors": "cache_errors",
        "nonCacheableRequests": "requests_not_cacheable",
    }
    metrics = {
        name: integer_total(stats.get(source)) for name, source in fields.items()
    }
    metrics["cacheSizeBytes"] = integer_total(payload.get("cache_size"))
    metrics["maxCacheSizeBytes"] = integer_total(payload.get("max_cache_size"))
    return metrics


def sccache_server_identity(output: str) -> dict[str, str | None]:
    """Return a bounded opaque identity for a fully described sccache backend."""

    try:
        payload = json.loads(output)
    except json.JSONDecodeError:
        return {"status": "unknown", "fingerprint": None}
    if not isinstance(payload, dict):
        return {"status": "unknown", "fingerprint": None}
    location = payload.get("cache_location")
    version = payload.get("version")
    if (
        not isinstance(location, str)
        or not isinstance(version, str)
        or not location
        or not version
        or len(location) > 512
        or len(version) > 128
    ):
        return {"status": "unknown", "fingerprint": None}
    identity = json.dumps(
        {"cacheLocation": location, "version": version},
        sort_keys=True,
        separators=(",", ":"),
    ).encode()
    fingerprint = hashlib.sha256(
        SCCACHE_IDENTITY_SALT.encode() + b"\0" + identity
    ).hexdigest()
    return {"status": "known", "fingerprint": fingerprint}


def read_sccache_stats() -> dict[str, Any]:
    executable = shutil.which("sccache")
    if executable is None:
        return {"status": "unavailable", "reason": "not-installed"}
    try:
        result = subprocess.run(
            [executable, "--show-stats", "--stats-format=json"],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=15,
        )
    except (OSError, subprocess.TimeoutExpired):
        return {"status": "unavailable", "reason": "stats-command-failed"}
    if result.returncode != 0:
        return {"status": "unavailable", "reason": "stats-command-failed"}
    try:
        return {
            "status": "available",
            "metrics": parse_sccache_stats(result.stdout),
            "serverIdentity": sccache_server_identity(result.stdout),
        }
    except FeedbackLatencyError:
        return {"status": "unavailable", "reason": "invalid-stats"}


def normalized_sccache_identity(value: object) -> dict[str, str | None]:
    if (
        isinstance(value, dict)
        and value.get("status") == "known"
        and isinstance(value.get("fingerprint"), str)
    ):
        return {"status": "known", "fingerprint": value["fingerprint"]}
    return {"status": "unknown", "fingerprint": None}


def sccache_delta(before: dict[str, Any], after: dict[str, Any]) -> dict[str, Any]:
    before_identity = normalized_sccache_identity(before.get("serverIdentity"))
    after_identity = normalized_sccache_identity(after.get("serverIdentity"))
    server_identity = {"before": before_identity, "after": after_identity}
    if before.get("status") != "available" or after.get("status") != "available":
        reason = after.get("reason") or before.get("reason") or "unavailable"
        return {
            "status": "unavailable",
            "reason": reason,
            "delta": None,
            "gauges": {},
            "serverIdentity": server_identity,
        }
    identity_statuses = {
        before_identity.get("status"),
        after_identity.get("status"),
    }
    identity_changed = (
        identity_statuses == {"known"}
        and before_identity.get("fingerprint") != after_identity.get("fingerprint")
    ) or identity_statuses == {"known", "unknown"}
    if identity_changed:
        return {
            "status": "server-changed",
            "reason": "sccache backend identity changed during the command",
            "delta": None,
            "gauges": {},
            "serverIdentity": server_identity,
        }
    before_metrics = before["metrics"]
    after_metrics = after["metrics"]
    delta: dict[str, int | float | None] = {}
    for name, after_value in after_metrics.items():
        if name in SCCACHE_GAUGES:
            continue
        before_value = before_metrics.get(name)
        if after_value is None or before_value is None:
            delta[name] = None
        elif after_value < before_value:
            return {
                "status": "counter-reset",
                "reason": "sccache counters decreased during the command",
                "delta": None,
                "gauges": {
                    gauge: after_metrics.get(gauge) for gauge in sorted(SCCACHE_GAUGES)
                },
                "serverIdentity": server_identity,
            }
        else:
            delta[name] = after_value - before_value
    hits = delta.get("cacheHits")
    misses = delta.get("cacheMisses")
    if isinstance(hits, int) and isinstance(misses, int) and hits + misses > 0:
        delta["hitRatePercent"] = round(100 * hits / (hits + misses), 2)
    return {
        "status": "available",
        "reason": None,
        "delta": delta,
        "gauges": {name: after_metrics.get(name) for name in sorted(SCCACHE_GAUGES)},
        "serverIdentity": server_identity,
    }


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def trusted_rusty_v8_preflight(
    repo_root: Path, environment: dict[str, str] | None = None
) -> dict[str, Any]:
    if environment is None:
        environment = os.environ
    expected: dict[str, str] = {}
    for manifest in sorted((repo_root / "third_party" / "v8").glob("*_release.sha256")):
        for line in manifest.read_text(encoding="utf-8").splitlines():
            parts = line.split()
            if len(parts) == 2 and len(parts[0]) == 64:
                expected[parts[1].lstrip("*")] = parts[0].lower()
    checked: list[str] = []
    failures: list[str] = []
    for variable in RUSTY_V8_ENV_VARS:
        raw_path = environment.get(variable)
        if not raw_path:
            failures.append(f"{variable} is not set")
            continue
        path = Path(raw_path)
        checked.append(variable)
        if not path.is_file():
            failures.append(f"{variable} does not name a file")
            continue
        checksum = expected.get(path.name)
        if checksum is None:
            failures.append(f"{variable} is not listed in a trusted checksum manifest")
        elif sha256_file(path) != checksum:
            failures.append(f"{variable} checksum does not match the trusted manifest")
    return {
        "status": "ready" if not failures else "failed",
        "checked": checked,
        "failures": failures,
    }


def helper_preflight(paths: list[str]) -> dict[str, Any]:
    failures: list[str] = []
    for raw_path in paths:
        path = Path(raw_path)
        if not path.is_file():
            failures.append("required helper is missing")
        elif os.name != "nt" and not os.access(path, os.X_OK):
            failures.append("required helper is not executable")
    return {
        "status": "ready" if not failures else "failed",
        "requiredCount": len(paths),
        "failures": failures,
    }


def normalize_exit_code(return_code: int) -> int:
    return 128 + abs(return_code) if return_code < 0 else min(return_code, 255)


def utc_now() -> str:
    return (
        datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")
    )


def safe_text(value: str) -> str:
    return value.replace("\n", " ").replace("\r", " ").replace("|", "\\|")[:120]


def render_summary(record: dict[str, Any]) -> str:
    cache = record["sccache"]
    source_edit = record.get("sourceEdit", {})
    hit_rate = "unavailable"
    if cache.get("status") == "available":
        hit_rate = str(cache["delta"].get("hitRatePercent", "n/a"))
    preflight = record["preflight"]
    lines = [
        "### Rust feedback latency",
        "",
        "| Field | Value |",
        "| --- | --- |",
        f"| Lane | `{safe_text(record['lane'])}` |",
        f"| Scenario | `{record['scenario']}` |",
        f"| Source | `{record['source']['commit']}` |",
        f"| Dirty | `{str(record['source']['dirty']).lower()}` |",
        f"| Source identity | `{source_edit.get('beforeStatus', 'unknown')} -> "
        f"{source_edit.get('afterStatus', 'unknown')}` |",
        f"| Duration | `{record['durationMs']} ms` |",
        f"| Command phase | `{record['phaseDurationsMs']['command']} ms` |",
        f"| Exit | `{record['exitCode']}` |",
        f"| sccache | `{cache['status']}` (hit rate: `{hit_rate}`) |",
        f"| Helper preflight | `{preflight['helpers']['status']}` |",
        f"| rusty_v8 preflight | `{preflight['rustyV8']['status']}` |",
        f"| Comparable | `{str(record['comparable']).lower()}` |",
        "",
    ]
    summary = "\n".join(lines)
    if len(summary.encode("utf-8")) > MAX_SUMMARY_BYTES:
        raise FeedbackLatencyError("generated Markdown summary exceeded its size limit")
    return summary


def write_evidence(record: dict[str, Any], output: Path) -> None:
    encoded = json.dumps(record, indent=2, sort_keys=True) + "\n"
    if len(encoded.encode("utf-8")) > MAX_JSON_BYTES:
        raise FeedbackLatencyError("generated JSON evidence exceeded its size limit")
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(encoded, encoding="utf-8")
    summary_path = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary_path:
        with Path(summary_path).open("a", encoding="utf-8") as handle:
            handle.write(render_summary(record))


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=__doc__,
        epilog=(
            "Scenarios are caller-declared: cold uses a fresh scratch build/cache; "
            "warm-cache uses a restored compiler cache with a fresh build tree; "
            "warm-noop immediately repeats unchanged inputs; warm-edit follows a "
            "declared source edit. The harness never infers or mutates cache state. "
            "Local sccache counters may include concurrent builds sharing the server."
        ),
    )
    parser.add_argument("--lane", required=True)
    parser.add_argument(
        "--scenario",
        required=True,
        choices=("cold", "warm-cache", "warm-noop", "warm-edit"),
    )
    parser.add_argument("--output", type=Path)
    parser.add_argument(
        "--configuration",
        default="unspecified",
        help="Public-safe build configuration label; caller-declared, not inferred",
    )
    parser.add_argument(
        "--storage-path",
        type=parse_path_spec,
        action="append",
        default=[],
        metavar="ROLE=PATH",
        help="Sample filesystem free space for at most eight explicit paths; no directory walk",
    )
    parser.add_argument(
        "--concurrent-builds",
        action="store_true",
        help="Uncontrolled other builds may contaminate this sample; mark it non-comparable",
    )
    parser.add_argument("--allow-dirty", action="store_true")
    parser.add_argument(
        "--expected-diff-sha256",
        type=parse_sha256,
        help=(
            "Expected git diff HEAD hash for an explicitly declared warm-edit scenario"
        ),
    )
    parser.add_argument("--require-helper", action="append", default=[])
    parser.add_argument("--verify-rusty-v8", action="store_true")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args(argv)
    if args.command and args.command[0] == "--":
        args.command = args.command[1:]
    if not args.command:
        parser.error("a command is required after --")
    if SAFE_LANE.fullmatch(args.lane) is None:
        parser.error(
            "--lane must be a public-safe name using letters, digits, ., _, or -"
        )
    if SAFE_LANE.fullmatch(args.configuration) is None:
        parser.error("--configuration must be a public-safe label")
    if args.expected_diff_sha256 and args.scenario != "warm-edit":
        parser.error("--expected-diff-sha256 requires --scenario warm-edit")
    if args.expected_diff_sha256 == EMPTY_DIFF_SHA256:
        parser.error("--expected-diff-sha256 must describe a non-empty tracked edit")
    if len(args.storage_path) > MAX_PATH_COUNT:
        parser.error("too many --storage-path arguments")
    paths = {}
    for role, path in args.storage_path:
        if role in paths:
            parser.error("--storage-path requires a unique public-safe ROLE=PATH")
        paths[role] = path
    args.storage_paths = paths
    return args


def main(argv: list[str]) -> int:
    args = parse_args(argv)
    try:
        started_at = utc_now()
        wall_started = time.monotonic()
        preflight_started = time.monotonic()
        source = source_identity(REPO_ROOT)
        if source["dirty"] and not args.allow_dirty and not args.expected_diff_sha256:
            raise FeedbackLatencyError(
                "refusing to measure a dirty checkout without --allow-dirty"
            )
        initial_source_edit = source_edit_evidence(
            source, source, args.expected_diff_sha256
        )
        if (
            args.expected_diff_sha256
            and initial_source_edit["beforeStatus"] != "matched"
        ):
            raise FeedbackLatencyError("declared source edit does not match checkout")
        helpers = helper_preflight(args.require_helper)
        rusty_v8 = (
            trusted_rusty_v8_preflight(REPO_ROOT)
            if args.verify_rusty_v8
            else {"status": "not-requested", "checked": [], "failures": []}
        )
        preflight = {"helpers": helpers, "rustyV8": rusty_v8}
        preflight_ready = all(
            result["status"] in {"ready", "not-requested"}
            for result in preflight.values()
        )
        preflight_duration_ms = round((time.monotonic() - preflight_started) * 1000)
        telemetry_started = time.monotonic()
        before_cache = read_sccache_stats()
        try:
            context = build_context(REPO_ROOT, args.command, args.configuration)
        except Exception:
            context = {"status": "unavailable"}
        sampler = StorageSampler(args.storage_paths)
        sampler.start()
        telemetry_duration_ms = round((time.monotonic() - telemetry_started) * 1000)
        command_started = time.monotonic()
        if preflight_ready:
            try:
                return_code = subprocess.run(args.command).returncode
                command_status = "completed"
            except OSError:
                print(
                    "feedback latency error: could not start measured command",
                    file=sys.stderr,
                )
                return_code = 127
                command_status = "failed-to-start"
            except KeyboardInterrupt:
                return_code = 130
                command_status = "interrupted"
        else:
            for group, result in preflight.items():
                for failure in result["failures"]:
                    print(
                        f"feedback latency {group} preflight: {failure}",
                        file=sys.stderr,
                    )
            return_code = HARNESS_EXIT_CODE
            command_status = "not-run"
        command_duration_ms = round((time.monotonic() - command_started) * 1000)
        source_check_started = time.monotonic()
        try:
            source_after = source_identity(REPO_ROOT)
        except FeedbackLatencyError:
            source_after = {"status": "unavailable"}
        telemetry_duration_ms += round((time.monotonic() - source_check_started) * 1000)
        telemetry_started = time.monotonic()
        storage = sampler.finish()
        cache = sccache_delta(before_cache, read_sccache_stats())
        telemetry_duration_ms += round((time.monotonic() - telemetry_started) * 1000)
        duration_ms = round((time.monotonic() - wall_started) * 1000)
        finished_at = utc_now()
        exit_code = normalize_exit_code(return_code)
        source_edit = source_edit_evidence(
            source, source_after, args.expected_diff_sha256
        )
        record = {
            "schemaVersion": SCHEMA_VERSION,
            "lane": args.lane[:120],
            "scenario": args.scenario,
            "source": source,
            "sourceAfter": source_after,
            "sourceEdit": source_edit,
            "environment": machine_identity(),
            "buildContext": context,
            "storage": storage,
            "concurrentBuildsDeclared": args.concurrent_builds,
            "cacheAttribution": "server-aggregate",
            "startedAt": started_at,
            "finishedAt": finished_at,
            "durationMs": duration_ms,
            "phaseDurationsMs": {
                "preflight": preflight_duration_ms,
                "command": command_duration_ms,
                "telemetry": telemetry_duration_ms,
            },
            "exitCode": exit_code,
            "commandStatus": command_status,
            "preflight": preflight,
            "sccache": cache,
            "comparable": source_edit["comparable"]
            and preflight_ready
            and cache["status"] not in {"counter-reset", "server-changed"}
            and not args.concurrent_builds
            and exit_code == 0
            and storage["status"] in {"available", "not-requested"}
            and context.get("status") != "unavailable",
        }
        output = args.output or Path(tempfile.gettempdir()) / (
            f"codex-feedback-{hashlib.sha256(args.lane.encode()).hexdigest()[:12]}.json"
        )
        write_evidence(record, output)
        print(f"feedback latency evidence: {output}")
        return exit_code
    except FeedbackLatencyError as error:
        print(f"feedback latency error: {error}", file=sys.stderr)
        return HARNESS_EXIT_CODE


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
