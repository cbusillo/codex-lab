#!/usr/bin/env python3
"""Small black-box harness for `codex exec --json` scenarios."""

import argparse
import http.server
import json
import os
import re
import shutil
import socketserver
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


HARNESS_DIR = Path(__file__).resolve().parent
if str(HARNESS_DIR) not in sys.path:
    sys.path.insert(0, str(HARNESS_DIR))

from terminal_outcome import (
    TerminalOutcome,
    model_failed,
    passed,
    policy_failed,
    runner_failed,
)


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUTPUT_ROOT = ROOT / ".tmp" / "codex-exec-harness"
AUTO_REVIEW_SUMMARY_MAX_FINDINGS = 20
AUTO_REVIEW_SUMMARY_MAX_FIELD_BYTES = 240
AUTO_REVIEW_SUMMARY_MAX_BYTES = 4096
AUTO_REVIEW_SUMMARY_OMITTED_TEMPLATE = "... {count} more finding(s) omitted"
ANCESTOR_GUIDANCE_FILENAMES = ("AGENTS.override.md", "AGENTS.md")
TOOL_LOOP_CONSECUTIVE_FAILURE_LIMIT = 3


class HarnessError(Exception):
    pass


def safe_path_component(value: str) -> str:
    sanitized = re.sub(r"[^A-Za-z0-9._-]+", "-", value).strip(".-")
    return sanitized or "scenario"


def require_safe_id(value: Any, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise HarnessError(f"{label} must be a non-empty string")
    if safe_path_component(value) != value:
        raise HarnessError(f"{label} contains unsafe path characters: {value!r}")
    return value


def truncate_utf8(value: str, max_bytes: int) -> str:
    encoded = value.encode("utf-8")
    if len(encoded) <= max_bytes:
        return value
    return encoded[:max_bytes].decode("utf-8", errors="ignore")


def resolve_under(root: Path, rel_path: str, description: str) -> Path:
    path = Path(rel_path)
    if path.is_absolute():
        raise HarnessError(f"{description} must be relative: {rel_path}")
    resolved_root = root.resolve()
    resolved_path = (root / path).resolve()
    if resolved_path != resolved_root and resolved_root not in resolved_path.parents:
        raise HarnessError(f"{description} escapes workspace: {rel_path}")
    return resolved_path


@dataclass(frozen=True)
class RunPaths:
    run_dir: Path
    workspace: Path
    codex_home: Path
    home: Path
    artifacts: Path


def save_text(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    opener = getattr(path, "open")
    with opener("w", encoding="utf-8") as handle:
        print(text, file=handle, end="")


def save_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    opener = getattr(path, "open")
    with opener("w", encoding="utf-8") as handle:
        json.dump(value, handle, indent=2, sort_keys=True)
        print(file=handle)


def copy_auth_files(source: Path, destination: Path) -> None:
    if not source.is_dir():
        raise HarnessError(f"cannot inherit missing auth home: {source}")
    destination.mkdir(parents=True, exist_ok=True)
    for name in ["auth.json", "auth-profiles.json", "auth-profiles"]:
        item = source / name
        if not item.exists():
            continue
        target = destination / item.name
        if item.is_dir():
            shutil.copytree(item, target, dirs_exist_ok=True)
        elif item.is_file():
            shutil.copy2(item, target)


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        raise HarnessError(f"{path}: invalid JSON: {exc}") from exc
    if not isinstance(value, dict):
        raise HarnessError(f"{path}: scenario must be a JSON object")
    return value


def contains_text(value: Any, needle: str) -> bool:
    if isinstance(value, str):
        return needle in value
    if isinstance(value, list):
        return any(contains_text(item, needle) for item in value)
    if isinstance(value, dict):
        return any(contains_text(item, needle) for item in value.values())
    return False


def count_text(value: Any, needle: str) -> int:
    if isinstance(value, str):
        return value.count(needle)
    if isinstance(value, list):
        return sum(count_text(item, needle) for item in value)
    if isinstance(value, dict):
        return sum(count_text(item, needle) for item in value.values())
    return 0


def sse_event(event_type: str, payload: dict[str, Any]) -> str:
    return f"event: {event_type}\ndata: {json.dumps(payload)}\n\n"


def completed_sse(response_id: str, usage: Any = None) -> str:
    return sse_event(
        "response.completed",
        {
            "type": "response.completed",
            "response": {
                "id": response_id,
                "usage": response_usage_payload(usage),
                "output": [],
            },
        },
    )


def response_usage_payload(usage: Any) -> dict[str, Any]:
    if not isinstance(usage, dict):
        usage = {}

    input_tokens = usage_token_value(usage, "input_tokens")
    output_tokens = usage_token_value(usage, "output_tokens")
    if "total_tokens" in usage:
        total_tokens = usage_token_value(usage, "total_tokens")
    else:
        total_tokens = input_tokens + output_tokens

    input_tokens_details = usage.get("input_tokens_details")
    if input_tokens_details is None and "cached_input_tokens" in usage:
        input_tokens_details = {
            "cached_tokens": usage_token_value(usage, "cached_input_tokens")
        }

    output_tokens_details = usage.get("output_tokens_details")
    if output_tokens_details is None and "reasoning_output_tokens" in usage:
        output_tokens_details = {
            "reasoning_tokens": usage_token_value(usage, "reasoning_output_tokens")
        }

    return {
        "input_tokens": input_tokens,
        "input_tokens_details": input_tokens_details,
        "output_tokens": output_tokens,
        "output_tokens_details": output_tokens_details,
        "total_tokens": total_tokens,
    }


def usage_token_value(usage: dict[str, Any], field: str) -> int:
    value = usage.get(field)
    if value is None:
        return 0
    if isinstance(value, bool) or not isinstance(value, int):
        raise HarnessError(f"responses_api usage {field} must be an integer")
    return value


def scenario_int(value: Any, label: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise HarnessError(f"{label} must be an integer")
    return value


def scenario_float(value: Any, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, int | float):
        raise HarnessError(f"{label} must be a number")
    return float(value)


def maybe_scenario_int(value: Any, label: str) -> int | None:
    if value is None:
        return None
    return scenario_int(value, label)


def maybe_scenario_float(value: Any, label: str) -> float | None:
    if value is None:
        return None
    return scenario_float(value, label)


def assert_not_self_prefix_match(
    request_index: int, other_request_index: int, label: str
) -> None:
    if request_index == other_request_index:
        raise HarnessError(f"{label} prefix_matches_request cannot reference itself")


def classify_auto_review_run(
    run: dict[str, Any], active_branch: str, active_head_sha: str
) -> str:
    target = run.get("target")
    if not isinstance(target, dict):
        return "detached"
    if target.get("branch") != active_branch:
        return "detached"
    if target.get("head_sha") != active_head_sha:
        return "stale"
    return "current"


def current_auto_review_findings(
    run: dict[str, Any], active_branch: str, active_head_sha: str
) -> list[dict[str, Any]]:
    if classify_auto_review_run(run, active_branch, active_head_sha) != "current":
        return []
    findings = run.get("findings")
    if not isinstance(findings, list):
        return []
    return [finding for finding in findings if isinstance(finding, dict)]


def render_auto_review_summary(
    run: dict[str, Any], active_branch: str, active_head_sha: str
) -> str:
    findings = current_auto_review_findings(run, active_branch, active_head_sha)
    if not findings:
        return ""
    lines: list[str] = []
    max_rendered_findings = min(len(findings), AUTO_REVIEW_SUMMARY_MAX_FINDINGS)

    for finding in findings[:max_rendered_findings]:
        finding_id = truncate_utf8(str(finding.get("id", "unknown")), 80)
        title = truncate_utf8(
            str(finding.get("title", "Untitled finding")),
            AUTO_REVIEW_SUMMARY_MAX_FIELD_BYTES,
        )
        priority = truncate_utf8(str(finding.get("priority", "unknown")), 20)
        location = truncate_utf8(
            str(finding.get("location", "unknown location")),
            AUTO_REVIEW_SUMMARY_MAX_FIELD_BYTES,
        )
        line = f"[{priority}] {finding_id}: {title} ({location})"
        omitted_after_candidate = len(findings) - (len(lines) + 1)
        reserved_bytes = 0
        if omitted_after_candidate > 0:
            omitted_line = AUTO_REVIEW_SUMMARY_OMITTED_TEMPLATE.format(
                count=omitted_after_candidate
            )
            reserved_bytes = len(omitted_line.encode("utf-8")) + 1
        remaining_bytes = max(0, AUTO_REVIEW_SUMMARY_MAX_BYTES - reserved_bytes)
        candidate_lines = [*lines, line]
        candidate = "\n".join(candidate_lines)
        if len(candidate.encode("utf-8")) > remaining_bytes:
            break
        lines = candidate_lines

    omitted_count = len(findings) - len(lines)
    if omitted_count > 0:
        omitted_line = AUTO_REVIEW_SUMMARY_OMITTED_TEMPLATE.format(count=omitted_count)
        lines.append(omitted_line)
    return truncate_utf8("\n".join(lines), AUTO_REVIEW_SUMMARY_MAX_BYTES)


def save_auto_review_run(sidecar_root: Path, run: dict[str, Any]) -> Path:
    run_id = require_safe_id(run.get("run_id"), "auto review run_id")
    path = sidecar_root / f"{run_id}.json"
    save_json(path, run)
    return path


def load_auto_review_run(sidecar_root: Path, run_id: str) -> dict[str, Any]:
    run_id = require_safe_id(run_id, "auto review run_id")
    path = sidecar_root / f"{run_id}.json"
    if not path.exists():
        raise HarnessError(f"unknown auto review run id: {run_id}")
    return load_json(path)


def auto_review_finding_detail(
    run: dict[str, Any], finding_id: str, max_bytes: int
) -> dict[str, Any]:
    if max_bytes <= 0:
        raise HarnessError("auto review detail max_bytes must be positive")
    findings = run.get("findings")
    if not isinstance(findings, list):
        raise HarnessError("auto review run has no findings")
    finding_id = str(finding_id)
    for finding in findings:
        if not isinstance(finding, dict) or str(finding.get("id")) != finding_id:
            continue
        payload = json.dumps(finding, indent=2, sort_keys=True)
        encoded = payload.encode("utf-8")
        truncated = len(encoded) > max_bytes
        if truncated:
            payload = truncate_utf8(payload, max_bytes)
        content_bytes = len(payload.encode("utf-8"))
        return {
            "finding_id": finding_id,
            "bytes": content_bytes,
            "original_bytes": len(encoded),
            "max_bytes": max_bytes,
            "truncated": truncated,
            "content": payload,
        }
    raise HarnessError(f"unknown auto review finding id: {finding_id}")


def load_durable_background_review_runs(codex_home: Path) -> list[dict[str, Any]]:
    review_root = codex_home / "state" / "review"
    runs_by_id: dict[str, dict[str, Any]] = {}
    runs_paths = sorted(review_root.glob("*/auto-review/runs.json"))
    runs_paths.extend(sorted(review_root.glob("*/store/runs.json")))
    for runs_path in runs_paths:
        index = load_json(runs_path)
        runs = index.get("runs", [])
        if not isinstance(runs, list):
            raise HarnessError(f"{runs_path}: runs must be a list")
        for run in runs:
            if not isinstance(run, dict):
                raise HarnessError(f"{runs_path}: run entries must be objects")
            run_id = run.get("run_id")
            if not isinstance(run_id, str) or not run_id:
                raise HarnessError(f"{runs_path}: run_id must be a non-empty string")
            runs_by_id[run_id] = run
    return list(runs_by_id.values())


def collect_workspace_git_state(workspace: Path) -> dict[str, Any]:
    def git_text(*args: str) -> str | None:
        completed = subprocess.run(
            ["git", "-C", str(workspace), *args],
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
        )
        if completed.returncode != 0:
            return None
        return completed.stdout.strip()

    status = git_text("status", "--porcelain")
    return {
        "branch": git_text("branch", "--show-current"),
        "git_root": git_text("rev-parse", "--show-toplevel"),
        "head_sha": git_text("rev-parse", "HEAD"),
        "worktree_path": str(workspace.resolve()),
        "clean": status == "" if status is not None else None,
    }


def response_sse_body(response: dict[str, Any]) -> str:
    if "sse" in response:
        return str(response["sse"])

    chunks: list[str] = []
    function_call = response.get("function_call")
    if function_call is not None:
        if not isinstance(function_call, dict):
            raise HarnessError("responses_api function_call must be an object")
        chunks.append(
            sse_event(
                "response.output_item.done",
                {
                    "type": "response.output_item.done",
                    "item": function_call,
                },
            )
        )

    for event in response.get("events", []):
        if not isinstance(event, dict):
            raise HarnessError("responses_api events must be objects")
        event_type = str(event.get("event", event.get("type", "response.output_item.done")))
        payload = event.get("payload")
        if payload is None and "item" in event:
            payload = {"type": "response.output_item.done", "item": event["item"]}
            event_type = "response.output_item.done"
        if not isinstance(payload, dict):
            raise HarnessError("responses_api event payload must be an object")
        chunks.append(sse_event(event_type, payload))

    if response.get("completed", True):
        chunks.append(
            completed_sse(
                str(response.get("response_id", "resp_harness")), response.get("usage")
            )
        )
    return "".join(chunks)


class FakeResponsesServer:
    def __init__(self, fixture: dict[str, Any]) -> None:
        responses = fixture.get("responses", [{}])
        if not isinstance(responses, list):
            raise HarnessError("responses_api.responses must be a list")
        if not responses:
            raise HarnessError("responses_api.responses must not be empty")
        self._responses = [r if isinstance(r, dict) else {} for r in responses]
        self.requests: list[dict[str, Any]] = []
        self._httpd: socketserver.TCPServer | None = None
        self._thread: threading.Thread | None = None

    def __enter__(self) -> "FakeResponsesServer":
        outer = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, format: str, *args: object) -> None:
                return

            def do_POST(self) -> None:
                length = int(self.headers.get("content-length", "0"))
                raw = self.rfile.read(length)
                try:
                    body: Any = json.loads(raw.decode("utf-8")) if raw else None
                except json.JSONDecodeError:
                    body = raw.decode("utf-8", errors="replace")

                outer.requests.append(
                    {
                        "path": self.path,
                        "headers": dict(self.headers.items()),
                        "body": body,
                    }
                )

                index = min(len(outer.requests) - 1, len(outer._responses) - 1)
                payload = response_sse_body(outer._responses[index]).encode("utf-8")
                self.send_response(200)
                self.send_header("content-type", "text/event-stream")
                self.send_header("content-length", str(len(payload)))
                self.end_headers()
                getattr(self.wfile, "write")(payload)

        self._httpd = socketserver.TCPServer(("127.0.0.1", 0), Handler)
        self._thread = threading.Thread(target=self._httpd.serve_forever, daemon=True)
        self._thread.start()
        return self

    def __exit__(self, _exc_type: object, _exc: object, _tb: object) -> None:
        if self._httpd is not None:
            self._httpd.shutdown()
            self._httpd.server_close()
        if self._thread is not None:
            self._thread.join(timeout=5)

    @property
    def base_url(self) -> str:
        if self._httpd is None:
            raise HarnessError("fake responses server is not running")
        host_value, port = self._httpd.server_address[:2]
        host = host_value.decode("utf-8") if isinstance(host_value, bytes) else host_value
        return f"http://{host}:{port}/v1"


def make_paths(
    output_root: Path, scenario_name: str, workspace_outside_git: bool = False
) -> RunPaths:
    stamp = time.strftime("%Y%m%d-%H%M%S")
    safe_name = safe_path_component(scenario_name)
    run_name = f"{stamp}-{safe_name}"
    run_dir = output_root / run_name
    external_workspace_root = Path(
        os.environ.get(
            "CODEX_EXEC_HARNESS_EXTERNAL_WORKSPACE_ROOT",
            Path.home() / ".codex-exec-harness-workspaces",
        )
    ).expanduser().resolve()
    workspace = (
        external_workspace_root / run_name
        if workspace_outside_git
        else run_dir / "workspace"
    )
    suffix = 1
    while run_dir.exists() or workspace.exists():
        suffix += 1
        run_name = f"{stamp}-{safe_name}-{suffix}"
        run_dir = output_root / run_name
        workspace = (
            external_workspace_root / run_name
            if workspace_outside_git
            else run_dir / "workspace"
        )
    return RunPaths(
        run_dir=run_dir,
        workspace=workspace,
        codex_home=run_dir / "codex-home",
        home=run_dir / "home",
        artifacts=run_dir / "artifacts",
    )


def workspace_lexical_path(root: Path, rel_path: str, label: str) -> Path:
    relative = Path(rel_path)
    if relative.is_absolute() or ".." in relative.parts:
        raise HarnessError(f"{label} escapes workspace: {rel_path}")
    return root / relative


def ancestor_guidance_paths(workspace: Path) -> list[Path]:
    return [
        candidate
        for ancestor in workspace.parents
        for filename in ANCESTOR_GUIDANCE_FILENAMES
        if (candidate := ancestor / filename).exists() or candidate.is_symlink()
    ]


def materialize_workspace(scenario: dict[str, Any], paths: RunPaths) -> None:
    paths.workspace.mkdir(parents=True, exist_ok=True)
    external_root = paths.run_dir / "external"
    external_files = scenario.get("external_files", {})
    if not isinstance(external_files, dict):
        raise HarnessError("external_files must be an object")
    for rel_path, content in external_files.items():
        if not isinstance(rel_path, str):
            raise HarnessError("external file paths must be strings")
        file_text = str(content).replace("{workspace}", str(paths.workspace))
        save_text(resolve_under(external_root, rel_path, "external file path"), file_text)

    files = scenario.get("files", {})
    if not isinstance(files, dict):
        raise HarnessError("files must be an object")
    for rel_path, content in files.items():
        if not isinstance(rel_path, str):
            raise HarnessError("file paths must be strings")
        file_text = str(content).replace("{workspace}", str(paths.workspace))
        save_text(resolve_under(paths.workspace, rel_path, "file path"), file_text)

    symlinks = scenario.get("symlinks", {})
    if not isinstance(symlinks, dict):
        raise HarnessError("symlinks must be an object")
    for rel_path, target_template in symlinks.items():
        if not isinstance(rel_path, str) or not isinstance(target_template, str):
            raise HarnessError("symlink paths and targets must be strings")
        link_path = workspace_lexical_path(paths.workspace, rel_path, "symlink path")
        target_text = (
            target_template.replace("{workspace}", str(paths.workspace))
            .replace("{external}", str(external_root))
            .replace("{run_dir}", str(paths.run_dir))
        )
        target_path = Path(target_text)
        if not target_path.is_absolute():
            target_path = paths.run_dir / target_path
        target_path = target_path.resolve(strict=True)
        run_root = paths.run_dir.resolve(strict=True)
        if not target_path.is_relative_to(run_root):
            raise HarnessError(f"symlink target escapes run directory: {target_path}")
        link_path.parent.mkdir(parents=True, exist_ok=True)
        link_path.symlink_to(target_path, target_is_directory=target_path.is_dir())

    executable_home_files = scenario.get("executable_home_files", {})
    if not isinstance(executable_home_files, dict):
        raise HarnessError("executable_home_files must be an object")
    for rel_path, content in executable_home_files.items():
        if not isinstance(rel_path, str):
            raise HarnessError("executable home file paths must be strings")
        file_text = (
            str(content)
            .replace("{workspace}", str(paths.workspace))
            .replace("{home}", str(paths.home))
        )
        executable = resolve_under(paths.home, rel_path, "executable home file path")
        save_text(executable, file_text)
        executable.chmod(0o755)

    if scenario.get("git_init", True):
        subprocess.run(
            ["git", "init", "-q"],
            cwd=paths.workspace,
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )


def inherit_auth_home(scenario: dict[str, Any], paths: RunPaths) -> None:
    if scenario.get("inherit_auth") is not True:
        return
    source = Path(os.environ.get("CODEX_LAB_HOME") or Path.home() / ".codex-lab")
    copy_auth_files(source, paths.codex_home)


def save_config(scenario: dict[str, Any], paths: RunPaths, base_url: str | None) -> None:
    config = str(scenario.get("config_toml", ""))
    config = config.replace("{workspace}", str(paths.workspace)).replace(
        "{home}", str(paths.home)
    )
    config = config.replace("{external}", str(paths.run_dir / "external")).replace(
        "{run_dir}", str(paths.run_dir)
    )
    uses_responses_base_url = "{responses_base_url}" in config
    if uses_responses_base_url:
        if base_url is None:
            raise HarnessError("config_toml uses {responses_base_url} without responses_api")
        config = config.replace("{responses_base_url}", base_url)
    if base_url is not None:
        if not uses_responses_base_url:
            config = config.rstrip() + "\n\n" if config.strip() else ""
            config += f'''model_provider = "harness"

[model_providers.harness]
name = "Harness"
base_url = "{base_url}"
wire_api = "responses"
requires_openai_auth = false
supports_websockets = false
'''
    if config:
        save_text(paths.codex_home / "config.toml", config)


def normalize_turns(scenario: dict[str, Any]) -> list[dict[str, Any]]:
    turns = scenario.get("turns")
    if turns is None:
        prompt = scenario.get("prompt")
        if not isinstance(prompt, str):
            raise HarnessError("prompt must be a string")
        return [{"prompt": prompt}]

    if not isinstance(turns, list) or not turns:
        raise HarnessError("turns must be a non-empty list")
    normalized = []
    for index, turn in enumerate(turns):
        if not isinstance(turn, dict):
            raise HarnessError(f"turns[{index}] must be an object")
        prompt = turn.get("prompt")
        if not isinstance(prompt, str):
            raise HarnessError(f"turns[{index}].prompt must be a string")
        normalized.append(turn)
    return normalized


def turn_artifacts(paths: RunPaths, turn_count: int, turn_index: int) -> Path:
    if turn_count == 1:
        return paths.artifacts
    artifact_dir = paths.artifacts / f"turn-{turn_index + 1:02d}"
    artifact_dir.mkdir(parents=True, exist_ok=True)
    return artifact_dir


def build_command(
    scenario: dict[str, Any],
    turn: dict[str, Any],
    codex_bin: str,
    paths: RunPaths,
    session_id: str | None,
) -> list[str]:
    prompt = turn.get("prompt")
    if not isinstance(prompt, str):
        raise HarnessError("prompt must be a string")

    resume = session_id is not None
    sandbox_value = scenario.get("sandbox", "danger-full-access")
    sandbox = str(sandbox_value) if sandbox_value is not None else None
    command = [
        codex_bin,
        "exec",
        "--json",
        "--skip-git-repo-check",
        "-C",
        str(paths.workspace),
    ]
    if sandbox is not None:
        command.extend(["--sandbox", sandbox])
    add_dirs = scenario.get("add_dirs", [])
    if not isinstance(add_dirs, list):
        raise HarnessError("add_dirs must be a list")
    for add_dir in add_dirs:
        if not isinstance(add_dir, str):
            raise HarnessError("add_dirs entries must be strings")
        add_dir_path = Path(
            add_dir.replace("{workspace}", str(paths.workspace))
            .replace("{external}", str(paths.run_dir / "external"))
            .replace("{run_dir}", str(paths.run_dir))
        ).resolve(strict=True)
        command.extend(["--add-dir", str(add_dir_path)])
    workspace_roots = scenario.get("workspace_roots", [])
    if not isinstance(workspace_roots, list):
        raise HarnessError("workspace_roots must be a list")
    if add_dirs and workspace_roots:
        raise HarnessError("workspace_roots cannot be combined with add_dirs")
    if workspace_roots and sandbox != "workspace-write":
        raise HarnessError("workspace_roots require sandbox=workspace-write")
    for workspace_root in workspace_roots:
        if not isinstance(workspace_root, str):
            raise HarnessError("workspace_roots entries must be strings")
        workspace_root_path = Path(
            workspace_root.replace("{workspace}", str(paths.workspace))
            .replace("{external}", str(paths.run_dir / "external"))
            .replace("{run_dir}", str(paths.run_dir))
        ).resolve(strict=True)
        command.extend(["--workspace-root", str(workspace_root_path)])
    if resume:
        command.append("resume")
    model = scenario.get("model")
    if isinstance(model, str) and model:
        command.extend(["-m", model])
    config_overrides = scenario.get("config_overrides", [])
    if not isinstance(config_overrides, list):
        raise HarnessError("config_overrides must be a list")
    for override in config_overrides:
        rendered_override = (
            str(override)
            .replace("{workspace}", str(paths.workspace))
            .replace("{external}", str(paths.run_dir / "external"))
            .replace("{run_dir}", str(paths.run_dir))
        )
        command.extend(["-c", rendered_override])
    if resume and session_id is not None:
        command.append(session_id)
    command.extend(["--", prompt])
    return command


def run_codex(
    command: list[str], scenario: dict[str, Any], paths: RunPaths, artifact_dir: Path
) -> dict[str, Any]:
    inherit_auth_home(scenario, paths)
    env = os.environ.copy()
    home = Path.home() if scenario.get("inherit_host_home") is True else paths.home
    env.update(
        {
            "CODEX_LAB_HOME": str(paths.codex_home),
            "CODEX_SQLITE_HOME": str(paths.codex_home),
            "HOME": str(home),
            "ZDOTDIR": str(paths.home),
            "XDG_CONFIG_HOME": str(paths.home / ".config"),
            "XDG_CACHE_HOME": str(paths.home / ".cache"),
        }
    )
    paths.home.mkdir(parents=True, exist_ok=True)
    paths.codex_home.mkdir(parents=True, exist_ok=True)

    stdout_lines: list[str] = []
    stderr_lines: list[str] = []
    events: list[dict[str, Any]] = []
    stdout_parse_errors = 0
    tool_loop_detected = False
    tool_loop_command: str | None = None
    last_failed_command: str | None = None
    failed_command_streak = 0
    timed_out = False

    try:
        proc = subprocess.Popen(
            command,
            cwd=paths.workspace,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=1,
        )
    except OSError as error:
        return {
            "returncode": 1,
            "events": [],
            "stderr": str(error),
            "launch_error": str(error),
            "timed_out": False,
            "stdout_parse_errors": 0,
            "tool_loop_detected": False,
            "tool_loop_command": None,
        }

    with proc:

        def read_stdout() -> None:
            nonlocal failed_command_streak
            nonlocal last_failed_command
            nonlocal stdout_parse_errors
            nonlocal tool_loop_command
            nonlocal tool_loop_detected
            assert proc.stdout is not None
            while True:
                line = proc.stdout.readline()
                if line == "":
                    break
                stdout_lines.append(line)
                try:
                    event = json.loads(line)
                except json.JSONDecodeError:
                    if line.endswith("\n"):
                        stdout_parse_errors += 1
                    event = {"unparsed": line.rstrip("\n")}
                events.append(event)
                if tool_loop_detected:
                    continue
                failed_commands = failed_commands_from_events([event])
                failed_command = failed_commands[-1] if failed_commands else None
                if failed_command is None:
                    continue
                if failed_command == last_failed_command:
                    failed_command_streak += 1
                else:
                    last_failed_command = failed_command
                    failed_command_streak = 1
                if failed_command_streak >= TOOL_LOOP_CONSECUTIVE_FAILURE_LIMIT:
                    tool_loop_detected = True
                    tool_loop_command = failed_command
                    proc.terminate()

        def read_stderr() -> None:
            assert proc.stderr is not None
            for line in proc.stderr:
                stderr_lines.append(line)

        stdout_thread = threading.Thread(target=read_stdout, daemon=True)
        stderr_thread = threading.Thread(target=read_stderr, daemon=True)
        stdout_thread.start()
        stderr_thread.start()

        try:
            proc.wait(timeout=int(scenario.get("timeout_seconds", 90)))
        except subprocess.TimeoutExpired:
            timed_out = True
            proc.kill()
            proc.wait()

        stdout_thread.join(timeout=5)
        stderr_thread.join(timeout=5)

        stdout = "".join(stdout_lines)
        stderr = "".join(stderr_lines)
        if timed_out:
            if not stdout and (artifact_dir / "stdout.jsonl").is_file():
                stdout = (artifact_dir / "stdout.jsonl").read_text(encoding="utf-8")
            if not stderr and (artifact_dir / "stderr.log").is_file():
                stderr = (artifact_dir / "stderr.log").read_text(encoding="utf-8")
            stderr += f"\nharness: command timed out after {int(scenario.get('timeout_seconds', 90))}s\n"
        returncode = 124 if timed_out else proc.returncode

    save_text(artifact_dir / "stdout.jsonl", stdout)
    save_text(artifact_dir / "stderr.log", stderr)

    return {
        "returncode": returncode,
        "events": events,
        "stderr": stderr,
        "timed_out": timed_out,
        "stdout_parse_errors": stdout_parse_errors,
        "tool_loop_detected": tool_loop_detected,
        "tool_loop_command": tool_loop_command,
    }


def extract_thread_id(events: list[dict[str, Any]]) -> str | None:
    for event in events:
        if event.get("type") == "thread.started" and isinstance(event.get("thread_id"), str):
            return event["thread_id"]
    return None


def event_type_counts(events: list[dict[str, Any]]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for event in events:
        event_type = event.get("type")
        if isinstance(event_type, str):
            counts[event_type] = counts.get(event_type, 0) + 1
    return counts


def agent_messages_from_events(events: list[dict[str, Any]]) -> list[str]:
    messages: list[str] = []
    for event in events:
        msg = event.get("msg")
        if isinstance(msg, dict) and msg.get("type") == "agent_message":
            message = msg.get("message")
            if isinstance(message, str):
                messages.append(message)
        item = event.get("item")
        if isinstance(item, dict) and item.get("type") == "agent_message":
            text = item.get("text")
            if isinstance(text, str):
                messages.append(text)
    return messages


def commands_from_events(events: list[dict[str, Any]]) -> list[str]:
    commands: list[str] = []
    for event in events:
        msg = event.get("msg")
        if isinstance(msg, dict) and msg.get("type") == "exec_command_begin":
            command = msg.get("command")
            if isinstance(command, list):
                commands.append(" ".join(str(part) for part in command))
            elif isinstance(command, str):
                commands.append(command)
        item = event.get("item")
        if isinstance(item, dict) and item.get("type") == "command_execution":
            command = item.get("command")
            if isinstance(command, str):
                commands.append(command)
    return commands


def failed_commands_from_events(events: list[dict[str, Any]]) -> list[str]:
    commands: list[str] = []
    for event in events:
        msg = event.get("msg")
        if isinstance(msg, dict) and msg.get("type") == "exec_command_end":
            exit_code = msg.get("exit_code")
            command = msg.get("command")
            if isinstance(exit_code, int) and exit_code != 0:
                if isinstance(command, list):
                    commands.append(" ".join(str(part) for part in command))
                elif isinstance(command, str):
                    commands.append(command)
        item = event.get("item")
        if isinstance(item, dict) and item.get("type") == "command_execution":
            status = item.get("status")
            exit_code = item.get("exit_code")
            if status == "failed" or (isinstance(exit_code, int) and exit_code != 0):
                command = item.get("command")
                if isinstance(command, list):
                    commands.append(" ".join(str(part) for part in command))
                elif isinstance(command, str):
                    commands.append(command)
    return commands


def agent_has_final_message(run: dict[str, Any]) -> bool:
    agent_messages = run.get("agent_messages")
    return isinstance(agent_messages, list) and bool(agent_messages)


def classify_execution_outcome(run: dict[str, Any]) -> TerminalOutcome | None:
    launch_error = run.get("launch_error")
    if isinstance(launch_error, str) and launch_error:
        return runner_failed("harness_error", truncate_utf8(launch_error, 240))

    if run.get("timed_out") is True:
        return runner_failed(
            "provider_timeout",
            truncate_utf8(f"command timed out after {run.get('timeout_seconds', 'unknown')}s", 240),
        )

    stderr = run.get("stderr")
    if isinstance(stderr, str) and stderr:
        lower_stderr = stderr.lower()
        if "connection refused" in lower_stderr or "could not connect" in lower_stderr:
            return runner_failed("provider_unavailable", truncate_utf8(stderr, 240))

    if run.get("tool_loop_detected") is True:
        loop_command = run.get("tool_loop_command")
        detail = (
            f"repeated identical failed tool call: {loop_command}"
            if isinstance(loop_command, str) and loop_command
            else "repeated identical failed tool call"
        )
        return model_failed("tool_loop", truncate_utf8(detail, 240))

    if int(run.get("stdout_parse_errors", 0) or 0) > 0:
        return runner_failed(
            "malformed_jsonl",
            truncate_utf8("model output was not valid JSONL", 240),
        )

    if not agent_has_final_message(run):
        return model_failed(
            "missing_final_message",
            truncate_utf8("scenario ended without a final assistant message", 240),
        )

    return None


def classify_terminal_outcome(
    run: dict[str, Any], failures: list[str]
) -> TerminalOutcome:
    execution_outcome = classify_execution_outcome(run)
    if execution_outcome is not None:
        return execution_outcome
    if failures:
        return policy_failed(truncate_utf8(failures[0], 240))

    return passed()


TOKEN_USAGE_FIELDS = [
    "input_tokens",
    "cached_input_tokens",
    "output_tokens",
    "reasoning_output_tokens",
    "total_tokens",
]


def empty_token_usage() -> dict[str, int]:
    return {field: 0 for field in TOKEN_USAGE_FIELDS}


def normalize_token_usage(usage: Any) -> dict[str, int]:
    tokens = empty_token_usage()
    if not isinstance(usage, dict):
        return tokens

    for field in TOKEN_USAGE_FIELDS:
        value = usage.get(field)
        if isinstance(value, bool):
            continue
        if isinstance(value, int):
            tokens[field] = value

    if tokens["total_tokens"] == 0:
        tokens["total_tokens"] = tokens["input_tokens"] + tokens["output_tokens"]
    return tokens


def add_token_usage(left: dict[str, int], right: dict[str, int]) -> dict[str, int]:
    return {field: left.get(field, 0) + right.get(field, 0) for field in TOKEN_USAGE_FIELDS}


def subtract_token_usage(left: dict[str, int], right: dict[str, int]) -> dict[str, int]:
    return {
        field: max(0, left.get(field, 0) - right.get(field, 0))
        for field in TOKEN_USAGE_FIELDS
    }


def token_usage_snapshot_from_events(events: list[dict[str, Any]]) -> dict[str, int]:
    snapshot = empty_token_usage()
    for event in events:
        if event.get("type") != "turn.completed":
            continue
        snapshot = normalize_token_usage(event.get("usage"))
    return snapshot


def run_turns(
    scenario: dict[str, Any],
    codex_bin: str,
    paths: RunPaths,
    requests: list[dict[str, Any]] | None,
) -> dict[str, Any]:
    turns = normalize_turns(scenario)
    turn_results = []
    all_events = []
    thread_id: str | None = None
    previous_token_usage = empty_token_usage()

    for index, turn in enumerate(turns):
        artifact_dir = turn_artifacts(paths, len(turns), index)
        command = build_command(scenario, turn, codex_bin, paths, thread_id)
        save_json(artifact_dir / "command.json", command)

        request_count_before = len(requests) if requests is not None else 0
        result = run_codex(command, scenario, paths, artifact_dir)
        request_count_after = len(requests) if requests is not None else 0
        result_thread_id = extract_thread_id(result["events"])
        if thread_id is None:
            thread_id = result_thread_id
        if index + 1 < len(turns) and thread_id is None:
            raise HarnessError(f"turn {index + 1} did not emit a thread.started event")

        all_events.extend(result["events"])
        agent_messages = agent_messages_from_events(result["events"])
        commands = commands_from_events(result["events"])
        token_usage_snapshot = token_usage_snapshot_from_events(result["events"])
        token_usage_delta = subtract_token_usage(token_usage_snapshot, previous_token_usage)
        previous_token_usage = token_usage_snapshot
        turn_results.append(
            {
                "index": index,
                "returncode": result["returncode"],
                "event_count": len(result["events"]),
                "event_types": event_type_counts(result["events"]),
                "responses_request_count": request_count_after - request_count_before,
                "thread_id": result_thread_id,
                "agent_messages": agent_messages,
                "commands": commands,
                "token_usage": token_usage_delta,
                "token_usage_snapshot": token_usage_snapshot,
                "stdout_parse_errors": result["stdout_parse_errors"],
                "stderr": result["stderr"],
                "launch_error": result.get("launch_error"),
                "tool_loop_detected": result["tool_loop_detected"],
                "tool_loop_command": result["tool_loop_command"],
                "timed_out": result["timed_out"],
                "artifact_dir": str(artifact_dir),
                "command": command,
            }
        )
        if result["returncode"] != 0:
            break

    return {
        "returncode": turn_results[-1]["returncode"],
        "events": all_events,
        "event_types": event_type_counts(all_events),
        "agent_messages": agent_messages_from_events(all_events),
        "commands": commands_from_events(all_events),
        "turns": turn_results,
        "thread_id": thread_id,
        "token_usage": previous_token_usage,
        "stdout_parse_errors": sum(int(turn.get("stdout_parse_errors", 0)) for turn in turn_results),
        "stderr": "".join(
            str(turn.get("stderr", "")) for turn in turn_results
        ),
        "launch_error": next(
            (
                turn.get("launch_error")
                for turn in turn_results
                if turn.get("launch_error")
            ),
            None,
        ),
        "timed_out": any(bool(turn.get("timed_out")) for turn in turn_results),
        "tool_loop_detected": any(
            bool(turn.get("tool_loop_detected")) for turn in turn_results
        ),
        "tool_loop_command": next(
            (
                turn.get("tool_loop_command")
                for turn in turn_results
                if turn.get("tool_loop_command")
            ),
            None,
        ),
        "timeout_seconds": scenario.get("timeout_seconds", 90),
        "workspace": str(paths.workspace),
        "run_dir": str(paths.run_dir),
    }


def scoped_request_body(request: dict[str, Any], scope: str) -> Any:
    body = request.get("body")
    if scope == "body":
        return body
    if isinstance(body, dict):
        return body.get(scope)
    return None


def request_tool_outputs(request: dict[str, Any], call_id: str) -> list[Any]:
    input_items = scoped_request_body(request, "input")
    if not isinstance(input_items, list):
        return []
    output_types = {"custom_tool_call_output", "function_call_output"}
    return [
        item.get("output")
        for item in input_items
        if isinstance(item, dict)
        and item.get("type") in output_types
        and item.get("call_id") == call_id
    ]


def canonical_text(value: Any) -> str:
    if isinstance(value, str):
        return value
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def add_text_assertion_failures(
    failures: list[str], subject: Any, assertion: dict[str, Any], label: str
) -> None:
    contains = assertion.get("contains")
    if isinstance(contains, str) and not contains_text(subject, contains):
        failures.append(f"{label}: missing {contains!r}")

    contains_all = assertion.get("contains_all", [])
    for needle in contains_all if isinstance(contains_all, list) else []:
        if not contains_text(subject, str(needle)):
            failures.append(f"{label}: missing {needle!r}")

    not_contains = assertion.get("not_contains")
    if isinstance(not_contains, str) and contains_text(subject, not_contains):
        failures.append(f"{label}: unexpectedly contained {not_contains!r}")

    counts = assertion.get("count", {})
    if isinstance(counts, dict):
        for needle, expected in counts.items():
            actual = count_text(subject, str(needle))
            expected_count = scenario_int(expected, f"{label} count for {needle!r}")
            if actual != expected_count:
                failures.append(
                    f"{label}: expected {needle!r} {expected} times, found {actual}"
                )


def add_absent_key_assertion_failures(
    failures: list[str], subject: Any, assertion: dict[str, Any], label: str
) -> None:
    absent_keys = assertion.get("absent_keys", [])
    if not isinstance(absent_keys, list):
        raise HarnessError(f"{label}.absent_keys must be a list")
    if not absent_keys:
        return
    if not isinstance(subject, dict):
        failures.append(f"{label}: missing object body for absent_keys assertion")
        return
    for key in absent_keys:
        if not isinstance(key, str) or not key:
            raise HarnessError(f"{label}.absent_keys entries must be non-empty strings")
        if key in subject:
            failures.append(f"{label}: unexpectedly contained key {key!r}")


def add_json_suffix_assertion_failures(
    failures: list[str], subject: Any, assertion: dict[str, Any], label: str
) -> None:
    json_suffix = assertion.get("json_suffix")
    if json_suffix is None:
        return
    if not isinstance(json_suffix, dict):
        raise HarnessError(f"{label}.json_suffix must be an object")
    if not isinstance(subject, str):
        failures.append(f"{label}.json_suffix: tool output is not text")
        return
    lines = [line for line in subject.splitlines() if line.strip()]
    if not lines:
        failures.append(f"{label}.json_suffix: tool output is empty")
        return
    try:
        parsed = json.loads(lines[-1])
    except json.JSONDecodeError as exc:
        failures.append(f"{label}.json_suffix: invalid JSON: {exc}")
        return
    add_text_assertion_failures(
        failures,
        canonical_text(parsed),
        json_suffix,
        f"{label}.json_suffix",
    )


def add_token_usage_assertion_failures(
    failures: list[str], usage: Any, assertion: Any, label: str
) -> None:
    if not isinstance(assertion, dict):
        raise HarnessError(f"{label} token_usage assertion must be an object")
    if not isinstance(usage, dict):
        usage = {}

    normalized = normalize_token_usage(usage)
    for field in TOKEN_USAGE_FIELDS:
        exact = maybe_scenario_int(assertion.get(field), f"{label}.{field}")
        if exact is not None and normalized[field] != exact:
            failures.append(
                f"{label}: expected {field} {exact}, found {normalized[field]}"
            )

        minimum = maybe_scenario_int(
            assertion.get(f"{field}_min"), f"{label}.{field}_min"
        )
        if minimum is not None and normalized[field] < minimum:
            failures.append(
                f"{label}: expected {field} >= {minimum}, found {normalized[field]}"
            )

        maximum = maybe_scenario_int(
            assertion.get(f"{field}_max"), f"{label}.{field}_max"
        )
        if maximum is not None and normalized[field] > maximum:
            failures.append(
                f"{label}: expected {field} <= {maximum}, found {normalized[field]}"
            )

    input_tokens = normalized["input_tokens"]
    cached_input_tokens = normalized["cached_input_tokens"]
    cache_ratio = cached_input_tokens / input_tokens if input_tokens > 0 else 0.0
    cache_ratio_min = maybe_scenario_float(
        assertion.get("cache_ratio_min"), f"{label}.cache_ratio_min"
    )
    if cache_ratio_min is not None and cache_ratio < cache_ratio_min:
        failures.append(
            f"{label}: expected cache_ratio >= {cache_ratio_min}, "
            f"found {cache_ratio:.3f}"
        )
    cache_ratio_max = maybe_scenario_float(
        assertion.get("cache_ratio_max"), f"{label}.cache_ratio_max"
    )
    if cache_ratio_max is not None and cache_ratio > cache_ratio_max:
        failures.append(
            f"{label}: expected cache_ratio <= {cache_ratio_max}, "
            f"found {cache_ratio:.3f}"
        )


def add_background_review_assertion_failures(
    failures: list[str], run: dict[str, Any], assertion: Any
) -> None:
    if assertion is None:
        return
    if not isinstance(assertion, dict):
        raise HarnessError("expect.background_review must be an object")

    runs = run.get("background_review_runs", [])
    if not isinstance(runs, list):
        runs = []
    expected_count = maybe_scenario_int(
        assertion.get("run_count"), "expect.background_review.run_count"
    )
    if expected_count is not None and len(runs) != expected_count:
        failures.append(
            f"expected {expected_count} durable auto review runs, found {len(runs)}"
        )
    if not runs:
        return
    if len(runs) != 1:
        failures.append(
            "auto review field assertions require exactly one durable run"
        )
        return

    durable_run = runs[0]
    for field in ["status", "source", "freshness"]:
        expected = assertion.get(field)
        if isinstance(expected, str) and durable_run.get(field) != expected:
            failures.append(
                f"auto review: expected {field} {expected!r}, found {durable_run.get(field)!r}"
            )

    expected_finding_count = maybe_scenario_int(
        assertion.get("finding_count"), "expect.background_review.finding_count"
    )
    if (
        expected_finding_count is not None
        and durable_run.get("finding_count") != expected_finding_count
    ):
        failures.append(
            "auto review: expected finding_count "
            f"{expected_finding_count}, found {durable_run.get('finding_count')!r}"
        )

    review_target = durable_run.get("review_target")
    expected_review_target_type = assertion.get("review_target_type")
    if isinstance(expected_review_target_type, str):
        actual_review_target_type = (
            review_target.get("type") if isinstance(review_target, dict) else None
        )
        if actual_review_target_type != expected_review_target_type:
            failures.append(
                "auto review: expected review_target.type "
                f"{expected_review_target_type!r}, found {actual_review_target_type!r}"
            )

    workspace_git = run.get("workspace_git")
    target = durable_run.get("target")
    if assertion.get("target_matches_workspace") is True:
        if not isinstance(workspace_git, dict) or not isinstance(target, dict):
            failures.append("auto review: missing workspace or target metadata")
        else:
            for field in ["branch", "head_sha", "worktree_path"]:
                if target.get(field) != workspace_git.get(field):
                    failures.append(
                        "auto review: target "
                        f"{field} {target.get(field)!r} does not match workspace "
                        f"{workspace_git.get(field)!r}"
                    )

    if assertion.get("worktree_clean") is True:
        if not isinstance(workspace_git, dict) or workspace_git.get("clean") is not True:
            failures.append("auto review: expected a clean workspace")
        if not isinstance(target, dict):
            failures.append("auto review: missing target metadata")
        elif target.get("worktree_diff_fingerprint") is not None:
            failures.append(
                "auto review: clean target unexpectedly recorded a worktree diff fingerprint"
            )


def add_list_text_assertion_failures(
    failures: list[str], values: Any, assertion: Any, label: str
) -> None:
    if assertion is None:
        return
    if not isinstance(assertion, dict):
        raise HarnessError(f"{label} assertion must be an object")
    if not isinstance(values, list):
        values = []
    text = "\n".join(str(value) for value in values)
    add_text_assertion_failures(failures, text, assertion, label)


def add_prefix_assertion_failures(
    failures: list[str],
    subject: Any,
    assertion: dict[str, Any],
    requests: list[dict[str, Any]],
    request_index: int,
    label: str,
) -> None:
    prefix_request = assertion.get("prefix_matches_request")
    if prefix_request is None:
        return

    other_request_index = scenario_int(prefix_request, f"{label}.prefix_matches_request")
    assert_not_self_prefix_match(request_index, other_request_index, label)
    if other_request_index < 0:
        failures.append(f"{label}: missing prefix request {other_request_index}")
        return
    if other_request_index >= len(requests):
        failures.append(f"{label}: missing prefix request {other_request_index}")
        return

    prefix_length = scenario_int(assertion.get("prefix_length", 0), f"{label}.prefix_length")
    if prefix_length <= 0:
        raise HarnessError(f"{label} prefix_length must be a positive integer")

    other_scope = str(assertion.get("prefix_scope", assertion.get("scope", "body")))
    other_subject = scoped_request_body(requests[other_request_index], other_scope)
    if subject is None:
        failures.append(f"{label}: missing prefix subject")
        return
    if other_subject is None:
        failures.append(
            f"{label}: missing prefix reference responses[{other_request_index}].{other_scope}"
        )
        return
    actual_prefix = canonical_text(subject)[:prefix_length]
    expected_prefix = canonical_text(other_subject)[:prefix_length]
    if actual_prefix != expected_prefix:
        failures.append(
            f"{label}: expected first {prefix_length} characters to match "
            f"responses[{other_request_index}].{other_scope}"
        )


def add_input_prefix_assertion_failures(
    failures: list[str],
    assertion: dict[str, Any],
    requests: list[dict[str, Any]],
    request_index: int,
    label: str,
) -> None:
    prefix_request = assertion.get("input_prefix_matches_request")
    if prefix_request is None:
        return

    other_request_index = scenario_int(
        prefix_request, f"{label}.input_prefix_matches_request"
    )
    assert_not_self_prefix_match(request_index, other_request_index, label)
    if other_request_index < 0:
        failures.append(f"{label}: missing input prefix request {other_request_index}")
        return
    if other_request_index >= len(requests):
        failures.append(f"{label}: missing input prefix request {other_request_index}")
        return

    subject = scoped_request_body(requests[request_index], "input")
    reference = scoped_request_body(requests[other_request_index], "input")
    if not isinstance(subject, list):
        failures.append(f"{label}: missing input prefix subject")
        return
    if not isinstance(reference, list):
        failures.append(
            f"{label}: missing input prefix reference responses[{other_request_index}].input"
        )
        return
    if len(subject) < len(reference):
        failures.append(
            f"{label}: expected input to have at least {len(reference)} prefix item(s), "
            f"found {len(subject)}"
        )
        return
    actual_prefix = subject[: len(reference)]
    if actual_prefix != reference:
        failures.append(
            f"{label}: expected input to start with responses[{other_request_index}].input"
        )


def add_workspace_path_assertion_failures(
    failures: list[str], run: dict[str, Any], assertions: Any
) -> None:
    if assertions is None:
        return
    if not isinstance(assertions, list):
        raise HarnessError("expect.workspace_paths must be a list")
    workspace_value = run.get("workspace")
    if not isinstance(workspace_value, str):
        raise HarnessError("run workspace is unavailable for path assertions")
    workspace = Path(workspace_value)
    for index, assertion in enumerate(assertions):
        label = f"workspace_paths[{index}]"
        if not isinstance(assertion, dict):
            raise HarnessError(f"expect.{label} must be an object")
        rel_path = assertion.get("path")
        if not isinstance(rel_path, str):
            raise HarnessError(f"expect.{label}.path must be a string")
        path = workspace_lexical_path(workspace, rel_path, label)
        exists = path.exists() or path.is_symlink()
        expected_exists = assertion.get("exists", True)
        if type(expected_exists) is not bool:
            raise HarnessError(f"expect.{label}.exists must be boolean")
        if exists != expected_exists:
            failures.append(f"{label}: expected exists={expected_exists}, found {exists}")
            continue
        if not exists:
            continue
        expected_type = assertion.get("type")
        if expected_type == "file" and not path.is_file():
            failures.append(f"{label}: expected file")
        elif expected_type == "directory" and not path.is_dir():
            failures.append(f"{label}: expected directory")
        elif expected_type == "symlink" and not path.is_symlink():
            failures.append(f"{label}: expected symlink")
        elif expected_type not in {None, "file", "directory", "symlink"}:
            raise HarnessError(f"expect.{label}.type is unsupported")
        if "contains" in assertion:
            if not path.is_file():
                failures.append(f"{label}: cannot inspect content of non-file")
                continue
            add_text_assertion_failures(
                failures,
                path.read_text(encoding="utf-8"),
                assertion,
                label,
            )


def evaluate_expectations(
    scenario: dict[str, Any], run: dict[str, Any], requests: list[dict[str, Any]]
) -> list[str]:
    failures: list[str] = []
    expect = scenario.get("expect", {})
    if not isinstance(expect, dict):
        raise HarnessError("expect must be an object")

    expected_returncode = maybe_scenario_int(expect.get("returncode"), "expect.returncode")
    if expected_returncode is not None and run["returncode"] != expected_returncode:
        failures.append(
            f"expected returncode {expected_returncode}, found {run['returncode']}"
        )

    expected_count = maybe_scenario_int(
        expect.get("responses_request_count"), "expect.responses_request_count"
    )
    if expected_count is not None and len(requests) != expected_count:
        failures.append(
            f"expected {expected_count} responses requests, found {len(requests)}"
        )

    expected_turn_count = maybe_scenario_int(expect.get("turn_count"), "expect.turn_count")
    if expected_turn_count is not None and len(run.get("turns", [])) != expected_turn_count:
        failures.append(
            f"expected {expected_turn_count} turns, found {len(run.get('turns', []))}"
        )

    if expect.get("thread_id") == "required" and not run.get("thread_id"):
        failures.append("expected a captured thread_id")

    workspace_git_assertion = expect.get("workspace_git")
    if workspace_git_assertion is not None:
        if not isinstance(workspace_git_assertion, dict):
            raise HarnessError("expect.workspace_git must be an object")
        workspace_git = run.get("workspace_git")
        if not isinstance(workspace_git, dict):
            failures.append("workspace_git: missing workspace Git evidence")
        else:
            for field, expected_value in workspace_git_assertion.items():
                if workspace_git.get(field) != expected_value:
                    failures.append(
                        f"workspace_git.{field}: expected {expected_value!r}, "
                        f"found {workspace_git.get(field)!r}"
                    )

    add_list_text_assertion_failures(
        failures,
        run.get("agent_messages"),
        expect.get("agent_messages"),
        "agent_messages",
    )
    add_list_text_assertion_failures(
        failures,
        run.get("commands"),
        expect.get("commands"),
        "commands",
    )
    launch_command_assertion = expect.get("launch_command")
    if launch_command_assertion is not None:
        if not isinstance(launch_command_assertion, dict):
            raise HarnessError("expect.launch_command must be an object")
        first_command = run.get("turns", [{}])[0].get("command", [])
        add_text_assertion_failures(
            failures,
            first_command,
            launch_command_assertion,
            "launch_command",
        )
    add_workspace_path_assertion_failures(failures, run, expect.get("workspace_paths"))
    add_background_review_assertion_failures(
        failures, run, expect.get("background_review")
    )

    if expect.get("same_thread_id") is True:
        expected_thread_id = run.get("thread_id")
        if not expected_thread_id:
            failures.append("expected a captured thread_id for same_thread_id")
        for index, turn in enumerate(run.get("turns", [])):
            if not isinstance(turn, dict):
                continue
            actual_thread_id = turn.get("thread_id")
            if actual_thread_id != expected_thread_id:
                failures.append(
                    f"turn {index}: expected thread_id {expected_thread_id!r}, "
                    f"found {actual_thread_id!r}"
                )

    event_types = expect.get("event_types", {})
    if not isinstance(event_types, dict):
        raise HarnessError("expect.event_types must be an object")
    actual_event_types = run.get("event_types", {})
    if not isinstance(actual_event_types, dict):
        actual_event_types = {}
    for event_type, expected in event_types.items():
        actual = actual_event_types.get(str(event_type), 0)
        expected_count = scenario_int(expected, f"expect.event_types.{event_type}")
        if actual != expected_count:
            failures.append(
                f"expected event type {event_type!r} {expected} times, found {actual}"
            )

    event_assertions = expect.get("events", [])
    if not isinstance(event_assertions, list):
        raise HarnessError("expect.events must be a list")
    actual_events = run.get("events", [])
    if not isinstance(actual_events, list):
        actual_events = []
    for index, assertion in enumerate(event_assertions):
        if not isinstance(assertion, dict):
            raise HarnessError("event assertions must be objects")
        event_type = assertion.get("type")
        if not isinstance(event_type, str) or not event_type:
            raise HarnessError(f"expect.events[{index}].type must be a non-empty string")
        matching_events = [
            event
            for event in actual_events
            if isinstance(event, dict) and event.get("type") == event_type
        ]
        occurrence = scenario_int(
            assertion.get("occurrence", 0), f"expect.events[{index}].occurrence"
        )
        if occurrence < 0 or occurrence >= len(matching_events):
            failures.append(
                f"event assertion {index}: missing occurrence {occurrence} of {event_type!r}"
            )
            continue
        subject = json.dumps(
            matching_events[occurrence], sort_keys=True, separators=(",", ":")
        )
        add_text_assertion_failures(
            failures,
            subject,
            assertion,
            f"events[{event_type!r}][{occurrence}]",
        )

    token_usage_assertion = expect.get("token_usage")
    if token_usage_assertion is not None:
        add_token_usage_assertion_failures(
            failures,
            run.get("token_usage"),
            token_usage_assertion,
            "token_usage",
        )

    turn_assertions = expect.get("turns", [])
    if not isinstance(turn_assertions, list):
        raise HarnessError("expect.turns must be a list")
    actual_turns = run.get("turns", [])
    if not isinstance(actual_turns, list):
        actual_turns = []
    for index, assertion in enumerate(turn_assertions):
        if not isinstance(assertion, dict):
            raise HarnessError("turn assertions must be objects")
        if index >= len(actual_turns) or not isinstance(actual_turns[index], dict):
            failures.append(f"turn assertion {index}: missing turn {index}")
            continue
        actual_turn = actual_turns[index]
        for key in ["returncode", "event_count", "responses_request_count"]:
            expected = maybe_scenario_int(assertion.get(key), f"turn {index}.{key}")
            if expected is not None and actual_turn.get(key) != expected:
                failures.append(
                    f"turn {index}: expected {key} {expected}, found {actual_turn.get(key)}"
                )
        if assertion.get("thread_id") == "required" and not actual_turn.get("thread_id"):
            failures.append(f"turn {index}: expected a captured thread_id")
        add_list_text_assertion_failures(
            failures,
            actual_turn.get("agent_messages"),
            assertion.get("agent_messages"),
            f"turn {index}.agent_messages",
        )
        add_list_text_assertion_failures(
            failures,
            actual_turn.get("commands"),
            assertion.get("commands"),
            f"turn {index}.commands",
        )
        token_usage_assertion = assertion.get("token_usage")
        if token_usage_assertion is not None:
            add_token_usage_assertion_failures(
                failures,
                actual_turn.get("token_usage"),
                token_usage_assertion,
                f"turn {index}.token_usage",
            )
        token_usage_snapshot_assertion = assertion.get("token_usage_snapshot")
        if token_usage_snapshot_assertion is not None:
            add_token_usage_assertion_failures(
                failures,
                actual_turn.get("token_usage_snapshot"),
                token_usage_snapshot_assertion,
                f"turn {index}.token_usage_snapshot",
            )
        turn_event_types = assertion.get("event_types", {})
        if not isinstance(turn_event_types, dict):
            raise HarnessError("turn event_types assertions must be objects")
        actual_turn_event_types = actual_turn.get("event_types", {})
        if not isinstance(actual_turn_event_types, dict):
            actual_turn_event_types = {}
        for event_type, expected in turn_event_types.items():
            actual = actual_turn_event_types.get(str(event_type), 0)
            expected_count = scenario_int(
                expected, f"turn {index}.event_types.{event_type}"
            )
            if actual != expected_count:
                failures.append(
                    f"turn {index}: expected event type {event_type!r} {expected} times, found {actual}"
                )

    response_assertions = expect.get("responses", [])
    if not isinstance(response_assertions, list):
        raise HarnessError("expect.responses must be a list")
    for index, assertion in enumerate(response_assertions):
        if not isinstance(assertion, dict):
            raise HarnessError("response assertions must be objects")
        request_index = scenario_int(assertion.get("request", index), f"responses[{index}].request")
        if request_index < 0 or request_index >= len(requests):
            failures.append(f"response assertion {index}: missing request {request_index}")
            continue
        scope = str(assertion.get("scope", "body"))
        subject = scoped_request_body(requests[request_index], scope)
        add_text_assertion_failures(
            failures, subject, assertion, f"responses[{request_index}].{scope}"
        )
        add_absent_key_assertion_failures(
            failures, subject, assertion, f"responses[{request_index}].{scope}"
        )
        add_prefix_assertion_failures(
            failures,
            subject,
            assertion,
            requests,
            request_index,
            f"responses[{request_index}].{scope}",
        )
        add_input_prefix_assertion_failures(
            failures,
            assertion,
            requests,
            request_index,
            f"responses[{request_index}]",
        )

    tool_output_assertions = expect.get("tool_outputs", [])
    if not isinstance(tool_output_assertions, list):
        raise HarnessError("expect.tool_outputs must be a list")
    for index, assertion in enumerate(tool_output_assertions):
        if not isinstance(assertion, dict):
            raise HarnessError("tool output assertions must be objects")
        request_index = scenario_int(
            assertion.get("request", index), f"tool_outputs[{index}].request"
        )
        if request_index < 0 or request_index >= len(requests):
            failures.append(
                f"tool output assertion {index}: missing request {request_index}"
            )
            continue
        call_id = assertion.get("call_id")
        if not isinstance(call_id, str) or not call_id:
            raise HarnessError(f"tool_outputs[{index}].call_id must be a non-empty string")
        outputs = request_tool_outputs(requests[request_index], call_id)
        label = f"tool_outputs[{index}] request {request_index} call_id {call_id!r}"
        if not outputs:
            failures.append(f"{label}: missing tool output")
            continue
        if len(outputs) > 1:
            failures.append(f"{label}: expected one tool output, found {len(outputs)}")
            continue
        add_text_assertion_failures(failures, outputs[0], assertion, label)
        add_json_suffix_assertion_failures(failures, outputs[0], assertion, label)

    return failures


def add_terminal_outcome_assertion_failures(
    failures: list[str], scenario: dict[str, Any], outcome: TerminalOutcome
) -> None:
    expect = scenario.get("expect", {})
    if not isinstance(expect, dict):
        raise HarnessError("expect must be an object")
    assertion = expect.get("terminal_outcome")
    if assertion is None:
        return
    if not isinstance(assertion, dict):
        raise HarnessError("expect.terminal_outcome must be an object")
    supported_fields = {"outcome", "reason", "detail_contains"}
    unknown_fields = set(assertion) - supported_fields
    if unknown_fields:
        raise HarnessError(
            "expect.terminal_outcome has unsupported fields: "
            + ", ".join(sorted(unknown_fields))
        )
    actual = {
        "outcome": outcome.outcome,
        "reason": outcome.reason,
        "detail": outcome.detail,
    }
    for field in ["outcome", "reason"]:
        expected = assertion.get(field)
        if expected is not None and not isinstance(expected, str):
            raise HarnessError(f"expect.terminal_outcome.{field} must be a string")
        if isinstance(expected, str) and actual[field] != expected:
            failures.append(
                f"terminal_outcome.{field}: expected {expected!r}, found {actual[field]!r}"
            )
    detail_contains = assertion.get("detail_contains")
    if detail_contains is not None and not isinstance(detail_contains, str):
        raise HarnessError("expect.terminal_outcome.detail_contains must be a string")
    actual_detail = actual["detail"]
    if isinstance(detail_contains, str) and (
        not isinstance(actual_detail, str) or detail_contains not in actual_detail
    ):
        failures.append(f"terminal_outcome.detail: missing {detail_contains!r}")


def run_scenario(args: argparse.Namespace) -> int:
    scenario_path = Path(args.scenario)
    scenario = load_json(scenario_path)
    name = str(scenario.get("name", scenario_path.stem))
    codex_bin = args.codex_bin or shutil.which("codex")
    if not codex_bin:
        raise HarnessError("codex binary not found; pass --codex-bin")

    workspace_outside_git = scenario.get("workspace_outside_git", False)
    if type(workspace_outside_git) is not bool:
        raise HarnessError("workspace_outside_git must be boolean")
    paths = make_paths(
        Path(args.output_root).resolve(),
        name,
        workspace_outside_git=workspace_outside_git,
    )
    paths.artifacts.mkdir(parents=True, exist_ok=True)
    materialize_workspace(scenario, paths)
    if workspace_outside_git:
        initial_git_state = collect_workspace_git_state(paths.workspace)
        if initial_git_state.get("git_root") is not None:
            raise HarnessError(
                "workspace_outside_git resolved inside a Git work tree: "
                f"{initial_git_state['git_root']}"
            )
        ancestor_guidance = ancestor_guidance_paths(paths.workspace)
        if ancestor_guidance:
            raise HarnessError(
                "workspace_outside_git resolved below ancestor guidance: "
                + ", ".join(str(path) for path in ancestor_guidance)
            )

    responses_api = scenario.get("responses_api")
    if responses_api is not None and not isinstance(responses_api, dict):
        raise HarnessError("responses_api must be an object")

    if responses_api is None:
        save_config(scenario, paths, None)
        run = run_turns(scenario, codex_bin, paths, requests=None)
        requests: list[dict[str, Any]] = []
    else:
        with FakeResponsesServer(responses_api) as server:
            save_config(scenario, paths, server.base_url)
            run = run_turns(scenario, codex_bin, paths, server.requests)
            requests = server.requests

    background_review_runs = load_durable_background_review_runs(paths.codex_home)
    workspace_git = collect_workspace_git_state(paths.workspace)
    run["background_review_runs"] = background_review_runs
    run["workspace_git"] = workspace_git
    failures = evaluate_expectations(scenario, run, requests)
    terminal_outcome = classify_terminal_outcome(run, failures)
    add_terminal_outcome_assertion_failures(failures, scenario, terminal_outcome)
    expect = scenario.get("expect", {})
    expected_terminal_outcome = (
        expect.get("terminal_outcome") if isinstance(expect, dict) else None
    )
    passed_run = not failures and (
        isinstance(expected_terminal_outcome, dict)
        or terminal_outcome.outcome == "passed"
    )
    save_json(paths.artifacts / "responses-requests.json", requests)
    save_json(paths.artifacts / "background-review-runs.json", background_review_runs)
    save_json(paths.artifacts / "workspace-git.json", workspace_git)
    summary = {
        "scenario": name,
        "scenario_path": str(scenario_path),
        "run_dir": str(paths.run_dir),
        "returncode": run["returncode"],
        "passed": passed_run,
        "failures": failures,
        "terminal_outcome": {
            "outcome": terminal_outcome.outcome,
            "reason": terminal_outcome.reason,
            "detail": terminal_outcome.detail,
        },
        "event_count": len(run["events"]),
        "event_types": run.get("event_types", {}),
        "responses_request_count": len(requests),
        "background_review_run_count": len(background_review_runs),
        "thread_id": run.get("thread_id"),
        "token_usage": run.get("token_usage", empty_token_usage()),
        "turns": run.get("turns", []),
        "stdout_parse_errors": run.get("stdout_parse_errors", 0),
        "tool_loop_detected": run.get("tool_loop_detected", False),
        "tool_loop_command": run.get("tool_loop_command"),
    }
    save_json(paths.artifacts / "summary.json", summary)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 0 if passed_run else 1


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("scenario", help="Path to a scenario JSON file")
    parser.add_argument("--codex-bin", help="Path to the codex binary under test")
    parser.add_argument(
        "--output-root",
        default=str(DEFAULT_OUTPUT_ROOT),
        help="Directory for isolated runs and artifacts",
    )
    return parser.parse_args(argv)


def main(argv: list[str]) -> int:
    try:
        return run_scenario(parse_args(argv))
    except subprocess.TimeoutExpired as exc:
        print(f"harness: command timed out after {exc.timeout}s", file=sys.stderr)
        return 124
    except HarnessError as exc:
        print(f"harness: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
