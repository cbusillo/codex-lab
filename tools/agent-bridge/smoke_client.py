#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["mcp>=2,<3"]
# ///
"""Drive agent_bridge.py over stdio the way an MCP host would. Spike check for #949."""

import asyncio
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

from mcp import ClientSession, StdioServerParameters
from mcp.client.stdio import stdio_client

BRIDGE = Path(__file__).with_name("agent_bridge.py")
PONG_TASK = "Reply with exactly the single word PONG and nothing else."


def payload(result) -> dict:
    if result.is_error:
        raise RuntimeError(result.content[0].text)
    return result.structured_content or json.loads(result.content[0].text)


async def check_pong(session: ClientSession, provider: str, workspace: str) -> bool:
    started = payload(await session.call_tool("spawn", {"provider": provider, "task": PONG_TASK, "cwd": workspace}))
    done = payload(await session.call_tool("wait", {"agent_id": started["agent_id"], "timeout_seconds": 300}))
    ok = done["status"] == "completed" and done["result"] == "PONG"
    print(f"{'ok  ' if ok else 'FAIL'} pong   {provider}: {done['status']} {done.get('result', '')[:80]!r} {done['elapsed_seconds']}s")
    return ok


async def check_cancel(workspace: str) -> bool:
    """Real agents background long commands and return, so cancel is exercised with a stand-in
    `claude` that holds a grandchild open, the shape of an agent running a tool."""
    shims = Path(workspace, "shims")
    shims.mkdir()
    marker = f"agent-bridge-cancel-{os.getpid()}"
    stand_in = shims / "claude"
    stand_in.write_text(f"#!/bin/sh\nsh -c 'exec -a {marker} sleep 300' &\nwait\n")
    stand_in.chmod(0o755)
    server = StdioServerParameters(
        command="uv",
        args=["run", "--script", str(BRIDGE)],
        env={**os.environ, "PATH": f"{shims}{os.pathsep}{os.environ['PATH']}"},
    )
    async with stdio_client(server) as (read, write), ClientSession(read, write) as session:
        await session.initialize()
        started = payload(await session.call_tool("spawn", {"provider": "claude", "task": "hold", "cwd": workspace}))
        await asyncio.sleep(2)
        running = subprocess.run(["pgrep", "-f", marker], capture_output=True, text=True).stdout.split()
        cancelled = payload(await session.call_tool("cancel", {"agent_id": started["agent_id"]}))
        leftovers = subprocess.run(["pgrep", "-f", marker], capture_output=True, text=True).stdout.split()
    ok = bool(running) and cancelled["status"] == "cancelled" and not leftovers
    print(f"{'ok  ' if ok else 'FAIL'} cancel stand-in: {cancelled['status']}; grandchild running before: {bool(running)}, left after: {leftovers}")
    return ok


async def main(providers: list[str]) -> int:
    workspace = tempfile.mkdtemp(prefix="agent-bridge-smoke-")
    server = StdioServerParameters(command="uv", args=["run", "--script", str(BRIDGE)])
    async with stdio_client(server) as (read, write), ClientSession(read, write) as session:
        await session.initialize()
        print("tools:", sorted(tool.name for tool in (await session.list_tools()).tools))
        results = [await check_pong(session, provider, workspace) for provider in providers]
    results.append(await check_cancel(workspace))
    return results.count(False)


if __name__ == "__main__":
    sys.exit(asyncio.run(main(sys.argv[1:] or ["claude", "antigravity"])))
