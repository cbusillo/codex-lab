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


ROOT = Path(__file__).resolve().parents[2]
DEFAULT_OUTPUT_ROOT = ROOT / ".tmp" / "codex-exec-harness"


class HarnessError(Exception):
    pass


def safe_path_component(value: str) -> str:
    sanitized = re.sub(r"[^A-Za-z0-9._-]+", "-", value).strip(".-")
    return sanitized or "scenario"


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


def completed_sse(response_id: str, usage: dict[str, Any] | None = None) -> str:
    usage_payload = usage or {
        "input_tokens": 0,
        "input_tokens_details": None,
        "output_tokens": 0,
        "output_tokens_details": None,
        "total_tokens": 0,
    }
    return sse_event(
        "response.completed",
        {
            "type": "response.completed",
            "response": {
                "id": response_id,
                "usage": usage_payload,
                "output": [],
            },
        },
    )


def response_sse_body(response: dict[str, Any]) -> str:
    if "sse" in response:
        return str(response["sse"])

    chunks: list[str] = []
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
        usage = response.get("usage")
        if usage is not None and not isinstance(usage, dict):
            raise HarnessError("responses_api response usage must be an object")
        chunks.append(
            completed_sse(str(response.get("response_id", "resp_harness")), usage)
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


def make_paths(output_root: Path, scenario_name: str) -> RunPaths:
    stamp = time.strftime("%Y%m%d-%H%M%S")
    safe_name = safe_path_component(scenario_name)
    run_dir = output_root / f"{stamp}-{safe_name}"
    suffix = 1
    while run_dir.exists():
        suffix += 1
        run_dir = output_root / f"{stamp}-{safe_name}-{suffix}"
    return RunPaths(
        run_dir=run_dir,
        workspace=run_dir / "workspace",
        codex_home=run_dir / "codex-home",
        home=run_dir / "home",
        artifacts=run_dir / "artifacts",
    )


def materialize_workspace(scenario: dict[str, Any], paths: RunPaths) -> None:
    paths.workspace.mkdir(parents=True, exist_ok=True)
    files = scenario.get("files", {})
    if not isinstance(files, dict):
        raise HarnessError("files must be an object")
    for rel_path, content in files.items():
        if not isinstance(rel_path, str):
            raise HarnessError("file paths must be strings")
        save_text(resolve_under(paths.workspace, rel_path, "file path"), str(content))

    if scenario.get("git_init", True):
        subprocess.run(
            ["git", "init", "-q"],
            cwd=paths.workspace,
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )


def save_config(scenario: dict[str, Any], paths: RunPaths, base_url: str | None) -> None:
    config = str(scenario.get("config_toml", ""))
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
    if resume:
        command = [codex_bin, "exec", "resume", "--json", "--skip-git-repo-check"]
    else:
        command = [
            codex_bin,
            "exec",
            "--json",
            "--skip-git-repo-check",
            "-C",
            str(paths.workspace),
            "--sandbox",
            str(scenario.get("sandbox", "danger-full-access")),
        ]
    model = scenario.get("model")
    if isinstance(model, str) and model:
        command.extend(["-m", model])
    config_overrides = scenario.get("config_overrides", [])
    if not isinstance(config_overrides, list):
        raise HarnessError("config_overrides must be a list")
    for override in config_overrides:
        command.extend(["-c", str(override)])
    if resume and session_id is not None:
        command.append(session_id)
    command.append(prompt)
    return command


def run_codex(
    command: list[str], scenario: dict[str, Any], paths: RunPaths, artifact_dir: Path
) -> dict[str, Any]:
    env = os.environ.copy()
    env.update(
        {
            "CODEX_HOME": str(paths.codex_home),
            "CODEX_SQLITE_HOME": str(paths.codex_home),
            "HOME": str(paths.home),
            "ZDOTDIR": str(paths.home),
            "XDG_CONFIG_HOME": str(paths.home / ".config"),
            "XDG_CACHE_HOME": str(paths.home / ".cache"),
        }
    )
    paths.home.mkdir(parents=True, exist_ok=True)
    paths.codex_home.mkdir(parents=True, exist_ok=True)

    try:
        completed = subprocess.run(
            command,
            cwd=paths.workspace,
            env=env,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=int(scenario.get("timeout_seconds", 90)),
        )
        stdout = completed.stdout
        stderr = completed.stderr
        returncode = completed.returncode
        timed_out = False
    except subprocess.TimeoutExpired as exc:
        stdout = exc.stdout or ""
        stderr = exc.stderr or ""
        if isinstance(stdout, bytes):
            stdout = stdout.decode("utf-8", errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode("utf-8", errors="replace")
        stderr += f"\nharness: command timed out after {exc.timeout}s\n"
        returncode = 124
        timed_out = True

    save_text(artifact_dir / "stdout.jsonl", stdout)
    save_text(artifact_dir / "stderr.log", stderr)
    events = []
    for line in stdout.splitlines():
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            events.append({"unparsed": line})

    return {
        "returncode": returncode,
        "events": events,
        "stderr": stderr,
        "timed_out": timed_out,
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
                "token_usage": token_usage_delta,
                "token_usage_snapshot": token_usage_snapshot,
                "artifact_dir": str(artifact_dir),
            }
        )
        if result["returncode"] != 0:
            break

    return {
        "returncode": turn_results[-1]["returncode"],
        "events": all_events,
        "event_types": event_type_counts(all_events),
        "turns": turn_results,
        "thread_id": thread_id,
        "token_usage": previous_token_usage,
    }


def scoped_request_body(request: dict[str, Any], scope: str) -> Any:
    body = request.get("body")
    if scope == "body":
        return body
    if isinstance(body, dict):
        return body.get(scope)
    return None


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
            if actual != int(expected):
                failures.append(
                    f"{label}: expected {needle!r} {expected} times, found {actual}"
                )


def add_token_usage_failures(
    failures: list[str], actual: Any, expected: Any, label: str
) -> None:
    if expected is None:
        return
    if not isinstance(expected, dict):
        raise HarnessError(f"{label}.token_usage must be an object")
    if not isinstance(actual, dict):
        actual = {}
    for field, expected_value in expected.items():
        actual_value = actual.get(str(field), 0)
        if actual_value != int(expected_value):
            failures.append(
                f"{label}: expected token_usage.{field} {expected_value}, "
                f"found {actual_value}"
            )


def evaluate_expectations(
    scenario: dict[str, Any], run: dict[str, Any], requests: list[dict[str, Any]]
) -> list[str]:
    failures: list[str] = []
    expect = scenario.get("expect", {})
    if not isinstance(expect, dict):
        raise HarnessError("expect must be an object")

    expected_returncode = expect.get("returncode")
    if expected_returncode is not None and run["returncode"] != int(expected_returncode):
        failures.append(
            f"expected returncode {expected_returncode}, found {run['returncode']}"
        )

    expected_count = expect.get("responses_request_count")
    if expected_count is not None and len(requests) != int(expected_count):
        failures.append(
            f"expected {expected_count} responses requests, found {len(requests)}"
        )

    expected_turn_count = expect.get("turn_count")
    if expected_turn_count is not None and len(run.get("turns", [])) != int(expected_turn_count):
        failures.append(
            f"expected {expected_turn_count} turns, found {len(run.get('turns', []))}"
        )

    add_token_usage_failures(
        failures, run.get("token_usage"), expect.get("token_usage"), "run"
    )

    if expect.get("thread_id") == "required" and not run.get("thread_id"):
        failures.append("expected a captured thread_id")

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
        if actual != int(expected):
            failures.append(
                f"expected event type {event_type!r} {expected} times, found {actual}"
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
            expected = assertion.get(key)
            if expected is not None and actual_turn.get(key) != int(expected):
                failures.append(
                    f"turn {index}: expected {key} {expected}, found {actual_turn.get(key)}"
                )
        if assertion.get("thread_id") == "required" and not actual_turn.get("thread_id"):
            failures.append(f"turn {index}: expected a captured thread_id")
        add_token_usage_failures(
            failures,
            actual_turn.get("token_usage"),
            assertion.get("token_usage"),
            f"turn {index}",
        )
        add_token_usage_failures(
            failures,
            actual_turn.get("token_usage_snapshot"),
            assertion.get("token_usage_snapshot"),
            f"turn {index} snapshot",
        )
        turn_event_types = assertion.get("event_types", {})
        if not isinstance(turn_event_types, dict):
            raise HarnessError("turn event_types assertions must be objects")
        actual_turn_event_types = actual_turn.get("event_types", {})
        if not isinstance(actual_turn_event_types, dict):
            actual_turn_event_types = {}
        for event_type, expected in turn_event_types.items():
            actual = actual_turn_event_types.get(str(event_type), 0)
            if actual != int(expected):
                failures.append(
                    f"turn {index}: expected event type {event_type!r} {expected} times, found {actual}"
                )

    response_assertions = expect.get("responses", [])
    if not isinstance(response_assertions, list):
        raise HarnessError("expect.responses must be a list")
    for index, assertion in enumerate(response_assertions):
        if not isinstance(assertion, dict):
            raise HarnessError("response assertions must be objects")
        request_index = int(assertion.get("request", index))
        if request_index >= len(requests):
            failures.append(f"response assertion {index}: missing request {request_index}")
            continue
        scope = str(assertion.get("scope", "body"))
        subject = scoped_request_body(requests[request_index], scope)
        add_text_assertion_failures(
            failures, subject, assertion, f"responses[{request_index}].{scope}"
        )

    return failures


def run_scenario(args: argparse.Namespace) -> int:
    scenario_path = Path(args.scenario)
    scenario = load_json(scenario_path)
    name = str(scenario.get("name", scenario_path.stem))
    codex_bin = args.codex_bin or shutil.which("codex")
    if not codex_bin:
        raise HarnessError("codex binary not found; pass --codex-bin")

    paths = make_paths(Path(args.output_root).resolve(), name)
    paths.artifacts.mkdir(parents=True, exist_ok=True)
    materialize_workspace(scenario, paths)

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

    failures = evaluate_expectations(scenario, run, requests)
    save_json(paths.artifacts / "responses-requests.json", requests)
    summary = {
        "scenario": name,
        "scenario_path": str(scenario_path),
        "run_dir": str(paths.run_dir),
        "returncode": run["returncode"],
        "passed": not failures,
        "failures": failures,
        "event_count": len(run["events"]),
        "event_types": run.get("event_types", {}),
        "responses_request_count": len(requests),
        "thread_id": run.get("thread_id"),
        "token_usage": run.get("token_usage", empty_token_usage()),
        "turns": run.get("turns", []),
    }
    save_json(paths.artifacts / "summary.json", summary)
    print(json.dumps(summary, indent=2, sort_keys=True))
    return 1 if failures else 0


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
