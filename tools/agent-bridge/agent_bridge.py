#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["mcp>=2,<3"]
# ///
"""Agent bridge: third-party coding agents as MCP tools.

Spike for cbusillo/codex-lab#949. Any MCP host (stock Codex, Claude Code) can
delegate a plain-text task to another vendor's CLI agent without engine support.
Provider command lines mirror `codex-rs/config/src/agent_defaults.rs` and
`codex-rs/core/src/agent/external_command.rs`.
"""

import asyncio
import atexit
import os
import shutil
import signal
import time
import uuid
from dataclasses import dataclass, field
from pathlib import Path

from mcp.server.mcpserver import MCPServer

MAX_TASK_BYTES = 64 * 1024
MAX_RESULT_BYTES = 64 * 1024
MAX_STDERR_BYTES = 4 * 1024
MAX_AGENTS = 8
DEFAULT_TIMEOUT_SECONDS = 1800
MAX_WAIT_SECONDS = 600
KILL_GRACE_SECONDS = 5

CLAUDE_READ_ONLY_TOOLS = (
    "Bash(ls:*), Bash(cat:*), Bash(grep:*), Bash(git status:*), Bash(git log:*), "
    "Bash(find:*), Read, Grep, Glob, LS, WebFetch, TodoRead, TodoWrite, WebSearch"
)


@dataclass(frozen=True)
class Provider:
    command: str
    read_only_args: tuple[str, ...]
    write_args: tuple[str, ...]
    adds_workspace_dir: bool


PROVIDERS = {
    "claude": Provider(
        command="claude",
        read_only_args=("--allowedTools", CLAUDE_READ_ONLY_TOOLS),
        write_args=("--dangerously-skip-permissions",),
        adds_workspace_dir=False,
    ),
    "antigravity": Provider(
        command="agy",
        read_only_args=("--sandbox", "--dangerously-skip-permissions", "--mode", "plan"),
        write_args=("--dangerously-skip-permissions",),
        adds_workspace_dir=True,
    ),
}


@dataclass
class Agent:
    agent_id: str
    provider: str
    process: asyncio.subprocess.Process
    started_at: float
    collector: asyncio.Task
    stdout: bytearray = field(default_factory=bytearray)
    stderr: bytearray = field(default_factory=bytearray)
    truncated: bool = False
    status: str = "running"


agents: dict[str, Agent] = {}
server = MCPServer("agent-bridge")


def describe(agent: Agent) -> dict:
    view = {
        "agent_id": agent.agent_id,
        "provider": agent.provider,
        "status": agent.status,
        "elapsed_seconds": round(time.monotonic() - agent.started_at, 1),
    }
    if agent.status != "running":
        view["exit_code"] = agent.process.returncode
        view["result"] = agent.stdout.decode("utf-8", errors="replace").strip()
        view["result_truncated"] = agent.truncated
        if agent.process.returncode != 0:
            view["stderr"] = agent.stderr.decode("utf-8", errors="replace").strip()
    return view


def terminate_group(agent: Agent, sig: signal.Signals) -> None:
    # Each agent leads its own session, so this reaches the tools it launched as well.
    try:
        os.killpg(agent.process.pid, sig)
    except ProcessLookupError:
        pass


async def collect(agent: Agent, timeout_seconds: int) -> None:
    async def drain(stream: asyncio.StreamReader, sink: bytearray, limit: int) -> None:
        while chunk := await stream.read(8192):
            room = limit - len(sink)
            if room > 0:
                sink.extend(chunk[:room])
            if len(chunk) > room:
                agent.truncated = True

    drains = asyncio.gather(
        drain(agent.process.stdout, agent.stdout, MAX_RESULT_BYTES),
        drain(agent.process.stderr, agent.stderr, MAX_STDERR_BYTES),
    )
    try:
        await asyncio.wait_for(asyncio.gather(drains, agent.process.wait()), timeout_seconds)
        if agent.status == "running":
            agent.status = "completed" if agent.process.returncode == 0 else "failed"
    except TimeoutError:
        agent.status = "timed_out"
        terminate_group(agent, signal.SIGTERM)
        try:
            await asyncio.wait_for(agent.process.wait(), KILL_GRACE_SECONDS)
        except TimeoutError:
            terminate_group(agent, signal.SIGKILL)
            await agent.process.wait()


@server.tool()
async def spawn(
    provider: str,
    task: str,
    cwd: str,
    mode: str = "read_only",
    model: str | None = None,
    timeout_seconds: int = DEFAULT_TIMEOUT_SECONDS,
) -> dict:
    """Start a third-party coding agent on a plain-text task and return its agent_id.

    provider: "claude" or "antigravity". cwd: absolute workspace directory. mode:
    "read_only" (default) or "write". The agent sees only `task`; include all context it
    needs. Call `wait` with the agent_id for the result.
    """
    spec = PROVIDERS.get(provider)
    if spec is None:
        raise ValueError(f"unknown provider {provider!r}; use one of {sorted(PROVIDERS)}")
    if mode not in ("read_only", "write"):
        raise ValueError('mode must be "read_only" or "write"')
    if not task.strip():
        raise ValueError("task is empty")
    if len(task.encode()) > MAX_TASK_BYTES:
        raise ValueError(f"task exceeds {MAX_TASK_BYTES} bytes")
    workspace = Path(cwd)
    if not workspace.is_absolute() or not workspace.is_dir():
        raise ValueError(f"cwd must be an existing absolute directory: {cwd}")
    if sum(agent.status == "running" for agent in agents.values()) >= MAX_AGENTS:
        raise ValueError(f"{MAX_AGENTS} agents are already running; wait or cancel first")
    executable = shutil.which(spec.command)
    if executable is None:
        raise ValueError(f"{spec.command} is not installed or not on PATH")

    args = list(spec.read_only_args if mode == "read_only" else spec.write_args)
    if model:
        args += ["--model", model]
    if spec.adds_workspace_dir:
        args += ["--add-dir", str(workspace)]
    args += ["-p", task]

    process = await asyncio.create_subprocess_exec(
        executable,
        *args,
        cwd=workspace,
        stdin=asyncio.subprocess.DEVNULL,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        start_new_session=True,
    )
    agent_id = uuid.uuid4().hex[:12]
    agent = Agent(agent_id, provider, process, time.monotonic(), collector=None)
    agent.collector = asyncio.create_task(collect(agent, max(1, timeout_seconds)))
    agents[agent_id] = agent
    return describe(agent)


@server.tool()
async def wait(agent_id: str, timeout_seconds: int = 120) -> dict:
    """Wait for an agent to finish, up to timeout_seconds, and return its status and result.

    A status of "running" means the wait elapsed; call again. The agent keeps running.
    """
    agent = agents.get(agent_id)
    if agent is None:
        raise ValueError(f"unknown agent_id {agent_id!r}")
    await asyncio.wait({agent.collector}, timeout=min(max(0, timeout_seconds), MAX_WAIT_SECONDS))
    return describe(agent)


@server.tool()
async def cancel(agent_id: str) -> dict:
    """Stop a running agent and every process it started."""
    agent = agents.get(agent_id)
    if agent is None:
        raise ValueError(f"unknown agent_id {agent_id!r}")
    if agent.status == "running":
        agent.status = "cancelled"
        terminate_group(agent, signal.SIGTERM)
        done, _ = await asyncio.wait({agent.collector}, timeout=KILL_GRACE_SECONDS)
        if not done:
            terminate_group(agent, signal.SIGKILL)
            await agent.collector
    return describe(agent)


@server.tool(name="list")
async def list_agents() -> list[dict]:
    """List agents started by this bridge with their current status."""
    return [describe(agent) for agent in agents.values()]


def stop_running_agents() -> None:
    # Agents lead their own sessions, so they would outlive the host that asked for them.
    for agent in agents.values():
        if agent.status == "running":
            terminate_group(agent, signal.SIGTERM)


if __name__ == "__main__":
    atexit.register(stop_running_agents)
    server.run()
