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
from .rpc import Rpc, RpcError
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
        if value is not None and not (key == "supports_standalone_web_search" and value is False)
    }


async def configure(rpc, port):
    value = {
        "name": "OpenAI",
        "base_url": f"http://127.0.0.1:{port}/backend-api/codex",
        "wire_api": "responses",
        "requires_openai_auth": True,
        "supports_websockets": False,
    }
    before = await rpc.call("config/read", {"includeLayers": True})
    current = provider_settings(before)
    if current == value:
        return {"provider": "account-router", "changed": False}
    if current is not None:
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


async def run(args):
    if args.command == "login":
        await enroll(args.data_dir, args.account, args.codex)
        return {"enrolled": args.account}
    if args.command == "serve":
        await serve(args)
        return None
    if args.command == "status":
        return await admin(args, "GET", "/status")
    if args.command == "select":
        return await admin(
            args, "POST", "/select", {"thread": args.thread, "execution": args.account}
        )
    rpc = await Rpc.local(args.control_socket)
    try:
        if args.command == "configure":
            return await configure(rpc, args.port)
        status = await admin(args, "GET", "/status")
        if args.account not in status["accounts"]:
            raise AccountError("unknown execution label")
        started = await rpc.call(
            "thread/start",
            {"modelProvider": "account-router", "cwd": str(args.cwd)},
        )
        thread_id = started["thread"]["id"]
        await admin(args, "POST", "/select", {"thread": thread_id, "execution": args.account})
        print(
            f"Task {thread_id} — execution {args.account}; phone/control login stays separate",
            flush=True,
        )
        # Keep the creator subscription alive while stock TUI attaches and runs.
        # This observer never answers approval or tool requests for that TUI.
        process = await asyncio.create_subprocess_exec(
            args.codex,
            "--remote",
            "unix://" + str(args.control_socket),
            "resume",
            thread_id,
            cwd=args.cwd,
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
    start.add_argument("--cwd", type=Path, default=Path.cwd())
    args = parser.parse_args()
    args.data_dir = args.data_dir.expanduser().absolute()
    args.control_socket = args.control_socket.expanduser().absolute()
    if args.command == "start":
        args.cwd = args.cwd.expanduser().resolve()
    try:
        result = asyncio.run(run(args))
        if result is not None:
            print(json.dumps(result, indent=2))
    except (AccountError, RpcError) as error:
        parser.exit(1, f"account-router: {error}\n")
    except (OSError, aiohttp.ClientError, TimeoutError):
        parser.exit(1, "account-router: local service, storage, or transport unavailable\n")
    except KeyboardInterrupt:
        sys.exit(130)
