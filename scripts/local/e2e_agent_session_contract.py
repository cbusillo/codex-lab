#!/usr/bin/env python3
"""Local cross-repo contract check for agent session migration.

This intentionally keeps the legacy Every Code rails in the contract. Codex Lab
is not ready to replace the active Every Code harness yet, so this check proves
the generic agent-session path is additive and still compatible.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess


EXPECTED_ENV = {
    "EVERY_CODE_SESSION_ORIGIN": "every_code",
    "EVERY_CODE_REQUEST_ID": "every-code-cbusillo-code-123-test",
    "EVERY_CODE_REPOSITORY": "cbusillo/code",
    "EVERY_CODE_ISSUE_NUMBER": "123",
    "EVERY_CODE_ISSUE_URL": "https://github.com/cbusillo/code/issues/123",
    "AGENT_SESSION_ORIGIN": "every_code",
    "AGENT_SESSION_REQUEST_ID": "every-code-cbusillo-code-123-test",
    "AGENT_SESSION_REPOSITORY": "cbusillo/code",
    "AGENT_SESSION_ISSUE_NUMBER": "123",
    "AGENT_SESSION_ISSUE_URL": "https://github.com/cbusillo/code/issues/123",
}


def repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def parse_args() -> argparse.Namespace:
    root = repo_root()
    developer_dir = Path.home() / "Developer"
    default_developer_dir = developer_dir if developer_dir.exists() else root.parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--launchplane", type=Path, default=default_developer_dir / "launchplane"
    )
    parser.add_argument(
        "--discord-blue", type=Path, default=default_developer_dir / "discord-blue"
    )
    parser.add_argument("--codex-lab", type=Path, default=root)
    parser.add_argument(
        "--skip-rust-test",
        action="store_true",
        help="Only validate sibling checkout contracts; do not run the Codex Lab Rust test.",
    )
    parser.add_argument(
        "--include-code-bridge-witness",
        action="store_true",
        help="Also run the local Code Bridge browser-app fixture witness test.",
    )
    return parser.parse_args()


def require_repo(path: Path, name: str) -> Path:
    resolved = path.expanduser().resolve()
    if not resolved.exists():
        raise SystemExit(f"{name} checkout not found: {resolved}")
    return resolved


def launchplane_session_env(launchplane: Path) -> dict[str, str]:
    probe = r"""
import json
import shlex
from pathlib import Path
from control_plane.contracts.every_code_work_request import EveryCodeWorkRequestRecord
from control_plane.every_code_worker import build_every_code_session_command

record = EveryCodeWorkRequestRecord(
    request_id="every-code-cbusillo-code-123-test",
    source="manual",
    state="queued",
    repository="cbusillo/code",
    issue_number=123,
    issue_url="https://github.com/cbusillo/code/issues/123",
    issue_title="Test issue",
    trigger_label="every-code",
    queued_at="2026-06-15T00:00:00Z",
    updated_at="2026-06-15T00:00:00Z",
)
command = build_every_code_session_command(
    record=record,
    command="code issue",
    state_dir=Path(".local-test-state"),
    host="contract-check",
    service_url="https://launchplane.example",
)
first_line = command.splitlines()[0]
env = {}
for token in shlex.split(first_line):
    if "=" not in token:
        break
    key, value = token.split("=", 1)
    if key.endswith(("_URL", "_ID", "_ORIGIN", "_REPOSITORY", "_NUMBER")):
        env[key] = value
print(json.dumps(env, sort_keys=True))
"""
    completed = subprocess.run(
        ["uv", "run", "python", "-c", probe],
        cwd=launchplane,
        check=True,
        stdout=subprocess.PIPE,
        text=True,
    )
    env = json.loads(completed.stdout)

    missing = {
        key: value for key, value in EXPECTED_ENV.items() if env.get(key) != value
    }
    if missing:
        formatted = ", ".join(
            f"{key}={value!r} got {env.get(key)!r}"
            for key, value in sorted(missing.items())
        )
        raise SystemExit(f"Launchplane session env contract mismatch: {formatted}")
    return env


def verify_discord_blue_routes(discord_blue: Path) -> None:
    probe = r"""
import os
from pathlib import Path
import tempfile
from aiohttp import web

home = tempfile.TemporaryDirectory()
os.environ["HOME"] = home.name
config_path = Path(home.name) / ".config" / "discord-blue" / "config.toml"
config_path.parent.mkdir(parents=True, exist_ok=True)
config_path.write_text(
    "\n".join(
        [
            "[discord]",
            'token = "from-contract-check"',
            "guild_id = 1",
            "bot_channel_id = 2",
            'employee_role_name = "employee"',
            "loaded_doodads = []",
            "",
            "[every_code]",
            "enabled = false",
            'listen_host = "0.0.0.0"',
            "listen_port = 8787",
            'token = ""',
            "channel_id = 0",
            'operator_role_name = ""',
            "auto_join_user_ids = []",
            "heartbeat_timeout_seconds = 120",
            "heartbeat_check_interval_seconds = 30",
        ]
    )
)

from discord_blue.config import Config
from discord_blue.doodads.every_code.bridge import EveryCodeBridge
from tests.fakes_every_code import FakeBot

bridge = EveryCodeBridge(FakeBot(Config()))
app = web.Application()
bridge.register_routes(app)
resources = {resource.canonical: resource for resource in app.router.resources()}
for path in ("/agent-session/connect", "/every-code/connect"):
    resource = resources.get(path)
    if resource is None:
        raise SystemExit(f"missing route {path}")
    route = next(iter(resource))
    if getattr(route.handler, "__self__", None) is not bridge:
        raise SystemExit(f"route {path} is not bound to the bridge")
    if getattr(route.handler, "__func__", None) is not bridge.handle_connect.__func__:
        raise SystemExit(f"route {path} does not use bridge.handle_connect")
print("discord-blue routes ok")
"""
    subprocess.run(["uv", "run", "python", "-c", probe], cwd=discord_blue, check=True)


def run_rust_tests(codex_lab: Path, env: dict[str, str]) -> None:
    protocol_cmd = [
        "cargo",
        "test",
        "-p",
        "codex-protocol",
        "session_source",
        "--no-fail-fast",
    ]
    subprocess.run(protocol_cmd, cwd=codex_lab / "codex-rs", check=True)

    generic_cmd = [
        "cargo",
        "test",
        "-p",
        "codex-tui",
        "embedded_app_server_startup_accepts_launchplane_agent_session_contract",
        "--no-fail-fast",
    ]
    subprocess.run(
        generic_cmd, cwd=codex_lab / "codex-rs", env={**os.environ, **env}, check=True
    )
    legacy_cmd = [
        "cargo",
        "test",
        "-p",
        "codex-tui",
        "embedded_app_server_startup_keeps_legacy_every_code_contract",
        "--no-fail-fast",
    ]
    subprocess.run(
        legacy_cmd, cwd=codex_lab / "codex-rs", env={**os.environ, **env}, check=True
    )


def run_code_bridge_browser_fixture_test(codex_lab: Path) -> None:
    bridge_cmd = [
        "cargo",
        "test",
        "-p",
        "codex-code-bridge-client",
        "browser_fixture_round_trips_nonblank_screenshot_and_control",
    ]
    subprocess.run(bridge_cmd, cwd=codex_lab / "codex-rs", check=True)


def main() -> int:
    args = parse_args()
    launchplane = require_repo(args.launchplane, "Launchplane")
    discord_blue = require_repo(args.discord_blue, "discord-blue")
    codex_lab = require_repo(args.codex_lab, "Codex Lab")

    env = launchplane_session_env(launchplane)
    verify_discord_blue_routes(discord_blue)
    if not args.skip_rust_test:
        run_rust_tests(codex_lab, env)
    if args.include_code_bridge_witness:
        run_code_bridge_browser_fixture_test(codex_lab)

    print("agent session contract ok")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
