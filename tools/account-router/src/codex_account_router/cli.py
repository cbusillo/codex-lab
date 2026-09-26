"""Owner commands usable locally or through a native SSH Shortcut."""

import argparse
import asyncio
import json
import os
import shutil
import sys
from pathlib import Path

import aiohttp

from .accounts import AccountError, enroll
from .phone import choices
from .rpc import ConnectionLost, Rpc, RpcError
from .server import serve


async def admin(args, method, path, payload=None):
    connector = aiohttp.UnixConnector(path=str(args.data_dir / "control.sock"))
    async with aiohttp.ClientSession(
        connector=connector, timeout=aiohttp.ClientTimeout(total=60)
    ) as session:
        async with session.request(method, "http://localhost" + path, json=payload) as response:
            result = await response.json()
            if response.status != 200:
                raise AccountError(result.get("error", "router command failed"))
            return result


def provider_settings(read):
    provider = (read["config"].get("model_providers") or {}).get("account-router")
    if not isinstance(provider, dict):
        return None
    # config/read expands optional provider fields and this stock default.
    return {
        key: value
        for key, value in provider.items()
        if value is not None
        and not (
            key in ("supports_standalone_web_search", "requires_openai_auth") and value is False
        )
    }


async def configure(rpc, port, auth_command):
    value = {
        "name": "OpenAI",
        "base_url": f"http://127.0.0.1:{port}/backend-api/codex",
        "wire_api": "responses",
        "auth": auth_command,
        "supports_websockets": False,
    }
    before = await rpc.call("config/read", {"includeLayers": True})
    current = provider_settings(before)
    if current == value:
        return {"provider": "account-router", "changed": False}
    if current is not None:
        relocated = dict(current)
        existing_auth = current.get("auth")
        if isinstance(existing_auth, dict):
            relocated["auth"] = dict(existing_auth, command=auth_command["command"])
        if relocated != value:
            raise AccountError("account-router provider already exists with different settings")
    user = next(
        (
            layer
            for layer in before["layers"]
            if layer["name"]["type"] == "user" and not layer["name"].get("profile")
        ),
        None,
    )
    if not isinstance(user, dict):
        raise AccountError("stock server did not report a writable user config layer")
    result = await rpc.call(
        "config/value/write",
        {
            "keyPath": "model_providers.account-router",
            "value": value,
            "mergeStrategy": "replace",
            "filePath": user["name"]["file"],
            "expectedVersion": user["version"],
        },
    )
    if result["status"] != "ok":
        raise AccountError("provider was written but is overridden by another configuration layer")
    after = await rpc.call("config/read", {"includeLayers": False})
    if provider_settings(after) != value:
        raise AccountError("provider write did not become effective")
    return {"provider": "account-router", "changed": True}


async def create_task(rpc, cwd, name):
    started = await rpc.call("thread/start", {"modelProvider": "account-router", "cwd": str(cwd)})
    thread_id = started["thread"]["id"]
    await rpc.call("thread/name/set", {"threadId": thread_id, "name": name})
    return thread_id


async def begin_task(rpc, thread_id, prompt):
    started = await rpc.call(
        "turn/start", {"threadId": thread_id, "input": [{"type": "text", "text": prompt}]}
    )
    # Stock cannot resume a new empty task. The owner's first input persists
    # its history asynchronously; wait for a successful same-server attachment.
    try:
        async with asyncio.timeout(10):
            for _ in range(50):
                try:
                    await rpc.call("thread/resume", {"threadId": thread_id, "excludeTurns": True})
                    return started["turn"]["id"]
                except ConnectionLost:
                    raise AccountError(
                        f"control connection lost; the first turn may still be running. "
                        f"Reconnect with resume {thread_id} and the same connection options"
                    ) from None
                except RpcError:
                    await asyncio.sleep(0.1)
    except TimeoutError:
        pass
    raise AccountError(
        f"the first turn may still be running; TUI attachment timed out. "
        f"Reconnect with resume {thread_id} and the same connection options"
    )


async def run(args):
    if args.command == "login":
        await enroll(args.data_dir, args.account, args.codex)
        return {"enrolled": args.account}
    if args.command == "serve":
        await serve(args)
        return None
    if args.command == "status":
        return await admin(args, "GET", "/status")
    if args.command in ("phone-list", "phone-select"):
        available = choices(await admin(args, "GET", "/status"))
        if args.command == "phone-list":
            if not available:
                raise AccountError("no idle routed tasks are available")
            return "\n".join(available)
        # Compare data with a fresh inventory. Never evaluate or interpolate
        # the task name as shell code, and refuse stale/busy selections.
        selected = available.get(sys.stdin.read(513).strip())
        if selected is None:
            raise AccountError("choice is unavailable; run the Shortcut again")
        return await admin(args, "POST", "/select", selected)
    if args.command == "select":
        return await admin(
            args, "POST", "/select", {"thread": args.thread, "execution": args.account}
        )
    rpc = await Rpc.local(args.control_socket)
    try:
        if args.command == "configure":
            return await configure(
                rpc,
                args.port,
                {
                    "command": sys.executable,
                    "args": ["-m", "codex_account_router.provider_auth", str(args.control_socket)],
                    "cwd": str(args.control_socket.parent),
                    "timeout_ms": 5000,
                    "refresh_interval_ms": 1,
                },
            )
        status = await admin(args, "GET", "/status")
        if args.command == "resume":
            thread = (
                await rpc.call("thread/read", {"threadId": args.thread, "includeTurns": False})
            )["thread"]
            if thread["id"] != args.thread or thread["modelProvider"] != "account-router":
                raise AccountError("task does not use the account-router provider")
            thread_id, cwd = args.thread, Path(thread["cwd"])
        else:
            if args.account not in status["accounts"]:
                raise AccountError("unknown execution label")
            cwd = args.cwd
            thread_id = await create_task(rpc, cwd, args.name or cwd.name or "New task")
            await admin(args, "POST", "/select", {"thread": thread_id, "execution": args.account})
            print(
                f"Task {thread_id} — execution {args.account}; phone/control login stays separate",
                flush=True,
            )
            await begin_task(rpc, thread_id, args.prompt)
        # Keep the creator subscription alive while stock TUI attaches and runs.
        # This observer never answers approval or tool requests for that TUI.
        config = await rpc.call("config/read", {"includeLayers": True})
        user = next(
            (
                layer
                for layer in config["layers"]
                if layer["name"]["type"] == "user" and not layer["name"].get("profile")
            ),
            None,
        )
        if user is None or not Path(user["name"]["file"]).is_absolute():
            raise AccountError("local server did not report its user config location")
        # --remote still loads client configuration locally. Match the socket's
        # stock home so a separate control host has the same named provider.
        tui_environment = dict(os.environ, CODEX_HOME=str(Path(user["name"]["file"]).parent))
        process = await asyncio.create_subprocess_exec(
            args.codex,
            "--remote",
            "unix://" + str(args.control_socket),
            "-c",
            'model_provider="account-router"',
            "resume",
            thread_id,
            cwd=cwd,
            env=tui_environment,
        )
        try:
            if await process.wait():
                raise AccountError(f"stock TUI exited; resume task {thread_id} to continue")
        finally:
            if process.returncode is None:
                process.terminate()
                await process.wait()
        return {"thread": thread_id}
    finally:
        await rpc.close()


def main():
    home = Path(os.environ.get("CODEX_HOME", str(Path.home() / ".codex")))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--data-dir", type=Path, default=home / "account-router")
    parser.add_argument(
        "--control-socket", type=Path, default=home / "app-server-control/app-server-control.sock"
    )
    parser.add_argument("--codex", default=shutil.which("codex") or "codex")
    parser.add_argument("--port", type=int, default=41979)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("configure", help="add the opt-in provider through stock config RPC")
    commands.add_parser("status", help="show task choices and last-request receipts as JSON")
    commands.add_parser("phone-list", help="list idle task/account choices, one per line")
    commands.add_parser("phone-select", help="select the exact phone-list choice from stdin")
    commands.add_parser(
        "login", help="run stock device login into an isolated execution home"
    ).add_argument("account")
    commands.add_parser("serve", help="run the router in the foreground").add_argument(
        "accounts", nargs="+"
    )
    select = commands.add_parser(
        "select", help="select an execution label for an idle opted-in task"
    )
    select.add_argument("thread")
    select.add_argument("account")
    start = commands.add_parser("start", help="create an opted-in task and attach stock TUI")
    start.add_argument("account")
    start.add_argument("prompt", nargs="?", help="first task request; prompted for when omitted")
    start.add_argument("--cwd", type=Path, default=Path.cwd())
    start.add_argument("--name", help="task name shown in the TUI and phone selector")
    commands.add_parser(
        "resume", help="reattach a routed task with the same provider"
    ).add_argument("thread")
    args = parser.parse_args()
    args.data_dir = args.data_dir.expanduser().absolute()
    args.control_socket = args.control_socket.expanduser().absolute()
    if args.command == "start":
        args.cwd = args.cwd.expanduser().resolve()
        try:
            args.prompt = args.prompt if args.prompt is not None else input("Task: ")
        except (EOFError, KeyboardInterrupt):
            parser.exit(1, "account-router: cancelled before creating a task\n")
        if not args.prompt.strip():
            parser.error("the first task request must not be empty")
    try:
        result = asyncio.run(run(args))
        if result is not None:
            print(result if isinstance(result, str) else json.dumps(result, indent=2))
    except (AccountError, RpcError) as error:
        parser.exit(1, f"account-router: {error}\n")
    except (OSError, aiohttp.ClientError, TimeoutError):
        parser.exit(1, "account-router: local service, storage, or transport unavailable\n")
    except KeyboardInterrupt:
        sys.exit(130)
