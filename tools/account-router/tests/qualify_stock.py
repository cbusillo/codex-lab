"""Installed stock Codex against the maintained router, synthetic accounts only."""

import argparse
import asyncio
import base64
import datetime
import json
import shlex
import sys
from contextlib import AsyncExitStack
from dataclasses import replace
from pathlib import Path

import aiohttp
import zstandard
from aiohttp import web

from codex_account_router.accounts import AccountWorker, account_home, worker_environment
from codex_account_router.cli import begin_task, configure, create_task
from codex_account_router.proxy import Proxy, application
from codex_account_router.rpc import RpcError
from codex_account_router.selection import Selection

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--codex", required=True, type=Path)
parser.add_argument("--output", required=True, type=Path)
args = parser.parse_args()
if sys.platform != "darwin":
    parser.error("the stock qualification probe currently requires macOS sandbox-exec")
CODEX = str(args.codex.resolve(strict=True))
POLICY = '(version 1) (allow default) (deny network-outbound) (allow network-outbound (remote ip "localhost:*"))'


def token(account):
    def encode(value):
        return base64.urlsafe_b64encode(json.dumps(value).encode()).decode().rstrip("=")

    claims = {
        "sub": "synthetic-user",
        "email": "synthetic@example.invalid",
        "exp": 2100000000,
        "https://api.openai.com/auth": {
            "chatgpt_account_id": account,
            "chatgpt_user_id": "synthetic-user",
            "chatgpt_plan_type": "plus",
        },
    }
    return f"{encode({'alg': 'none'})}.{encode(claims)}.synthetic"


class Rpc:
    def __init__(self, process):
        self.stdin, self.stdout = process.stdin, process.stdout
        assert self.stdin is not None and self.stdout is not None
        self.counter = 0
        self.pending: dict[int, asyncio.Future[dict]] = {}
        self.events = asyncio.Queue()
        self.reader = asyncio.create_task(self.read())

    async def read(self):
        while line := await self.stdout.readline():
            obj = json.loads(line)
            if "id" in obj and obj["id"] in self.pending:
                self.pending.pop(obj["id"]).set_result(obj)
            else:
                await self.events.put(obj)
        for future in self.pending.values():
            if not future.done():
                future.set_exception(
                    RuntimeError("app-server closed stdout; inspect synthetic stderr")
                )

    async def send(self, obj):
        self.stdin.write((json.dumps(obj) + "\n").encode())
        await self.stdin.drain()

    async def call(self, method, params):
        self.counter += 1
        future = asyncio.get_running_loop().create_future()
        self.pending[self.counter] = future
        await self.send({"id": self.counter, "method": method, "params": params})
        obj = await asyncio.wait_for(future, 30)
        if "error" in obj:
            raise RpcError(f"{method}: {obj['error']}")
        return obj["result"]

    async def turn(self, thread, text):
        start = await self.call(
            "turn/start", {"threadId": thread, "input": [{"type": "text", "text": text}]}
        )
        turn_id = start["turn"]["id"]
        return await self.completed(turn_id)

    async def completed(self, turn_id):
        async with asyncio.timeout(60):
            while True:
                obj = await self.events.get()
                params = obj.get("params", {})
                if (
                    obj.get("method") == "turn/completed"
                    and params.get("turn", {}).get("id") == turn_id
                ):
                    return params["turn"]


def identity(bearer):
    encoded = bearer.split(".")[1]
    return json.loads(base64.urlsafe_b64decode(encoded + "=" * (-len(encoded) % 4)))[
        "https://api.openai.com/auth"
    ]["chatgpt_account_id"]


async def listen(stack, app):
    runner = web.AppRunner(app, access_log=None, auto_decompress=False)
    await runner.setup()
    stack.push_async_callback(runner.cleanup)
    site = web.TCPSite(runner, "127.0.0.1", 0)
    await site.start()
    return f"http://127.0.0.1:{runner.addresses[0][1]}"


async def close_stock(process, rpc):
    if process.returncode is None:
        input_stream = process.stdin
        assert input_stream is not None
        input_stream.close()
        await asyncio.wait_for(process.wait(), 10)
    rpc.reader.cancel()
    await asyncio.gather(rpc.reader, return_exceptions=True)


async def main():
    run = args.output.absolute()
    run.mkdir(mode=0o700)
    state = {"limit": False, "reject": False}
    requests: list[dict] = []
    refreshes: list[str] = []

    async def upstream(request):
        if request.path.endswith("/models"):
            return web.json_response({"models": []})
        body = await request.read()
        if request.headers.get("Content-Encoding") == "zstd":
            body = zstandard.ZstdDecompressor().decompress(body, max_output_size=64 * 1024 * 1024)
        payload = json.loads(body)
        account = request.headers["ChatGPT-Account-Id"]
        assert identity(request.headers["Authorization"]) == account
        requests.append({"account": account, "payload": payload})
        if state["reject"]:
            return web.json_response({"error": {"message": "synthetic auth error"}}, status=401)
        if state["limit"] and account == "first":
            return web.json_response(
                {
                    "error": {
                        "type": "usage_limit_reached",
                        "message": "synthetic limit",
                        "plan_type": "plus",
                        "resets_at": 2100000000,
                    }
                },
                status=429,
            )
        index = len(requests)
        message = {
            "id": f"message-{index}",
            "type": "message",
            "role": "assistant",
            "status": "completed",
            "content": [{"type": "output_text", "text": f"{account}_OK", "annotations": []}],
        }
        events = [
            {"type": "response.created", "response": {"id": f"response-{index}"}},
            {"type": "response.output_item.done", "output_index": 0, "item": message},
            {
                "type": "response.completed",
                "response": {
                    "id": f"response-{index}",
                    "status": "completed",
                    "output": [message],
                    "usage": {"input_tokens": 10, "output_tokens": 2, "total_tokens": 12},
                },
            },
        ]
        response = web.StreamResponse(headers={"Content-Type": "text/event-stream"})
        await response.prepare(request)
        for event in events:
            await response.write(f"event: {event['type']}\ndata: {json.dumps(event)}\n\n".encode())
            await asyncio.sleep(0.01)
        await response.write_eof()
        return response

    async def accounts(request):
        account = identity(request.headers["Authorization"])
        return web.json_response(
            {
                "accounts": [
                    {
                        "id": account,
                        "workspace_backend_origin": "https://chatgpt.com",
                        "account_routing_override": "NO_CONSTRAINT",
                    }
                ]
            }
        )

    async def refresh(request):
        body = await request.json()
        account = body["refresh_token"].split(":")[1]
        refreshes.append(account)
        return web.json_response(
            {
                "access_token": token(account),
                "id_token": token(account),
                "refresh_token": f"synthetic:{account}:rotated",
                "token_type": "Bearer",
            }
        )

    async with AsyncExitStack() as stack:
        backend = web.Application()
        backend.router.add_post("/oauth/token", refresh)
        backend.router.add_get("/backend-api/wham/accounts/check", accounts)
        backend.router.add_route("*", "/backend-api/codex/{tail:.*}", upstream)
        backend_url = await listen(stack, backend)
        wrapper = run / "synthetic-codex"
        wrapper.write_text(
            "#!/bin/sh\nexport CODEX_REFRESH_TOKEN_URL_OVERRIDE="
            + shlex.quote(backend_url + "/oauth/token")
            + "\nexec /usr/bin/sandbox-exec -p "
            + shlex.quote(POLICY)
            + " "
            + shlex.quote(CODEX)
            + ' "$@"\n'
        )
        wrapper.chmod(0o700)

        def prepare(fixture_home, account):
            fixture_home.mkdir(mode=0o700, exist_ok=True)
            (fixture_home / "config.toml").write_text(
                f'model = "gpt-5.4"\nchatgpt_base_url = "{backend_url}/backend-api"\ncli_auth_credentials_store = "file"\napproval_policy = "never"\nsandbox_mode = "read-only"\n[analytics]\nenabled = false\n[feedback]\nenabled = false\n[features]\nremote_control = false\napps = false\n'
            )
            auth = {
                "auth_mode": "chatgpt",
                "OPENAI_API_KEY": None,
                "tokens": {
                    "access_token": token(account),
                    "id_token": token(account),
                    "refresh_token": f"synthetic:{account}:old",
                    "account_id": account,
                },
                "last_refresh": datetime.datetime.now(datetime.UTC).isoformat(),
            }
            (fixture_home / "auth.json").write_text(json.dumps(auth))
            (fixture_home / "auth.json").chmod(0o600)

        data = run / "router"
        workers = {}
        for label in ("first", "second"):
            home = account_home(data, label)
            prepare(home, label)
            worker = await AccountWorker.start(data, label, str(wrapper))
            stack.push_async_callback(worker.close)
            credential = await worker.credentials()
            assert credential.account_id == label

            class LoopbackWorker:
                def __init__(self, credential_worker):
                    self.worker = credential_worker

                async def credentials(self, **kwargs):
                    return replace(await self.worker.credentials(**kwargs), origin=backend_url)

            workers[label] = LoopbackWorker(worker)
        session = await stack.enter_async_context(aiohttp.ClientSession(auto_decompress=False))
        proxy = Proxy(None, None, workers, session)
        router_url = await listen(stack, application(proxy))
        control_home = run / "control"
        prepare(control_home, "control")
        control_auth = (control_home / "auth.json").read_bytes()
        token_helper = run / "synthetic-caller-auth.py"
        token_helper.write_text(f"print({token('control')!r})\n")
        auth_command = {
            "command": sys.executable,
            "args": [str(token_helper)],
            "cwd": str(control_home),
            "timeout_ms": 5000,
            "refresh_interval_ms": 1,
        }

        async def start_control():
            control_process = await asyncio.create_subprocess_exec(
                str(wrapper),
                "app-server",
                "--stdio",
                cwd=control_home,
                env=worker_environment(control_home),
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.DEVNULL,
            )
            control_rpc = Rpc(control_process)
            stack.push_async_callback(close_stock, control_process, control_rpc)
            await control_rpc.call(
                "initialize",
                {
                    "clientInfo": {"name": "maintained_router_probe", "version": "0.1"},
                    "capabilities": {"experimentalApi": True},
                },
            )
            await control_rpc.send({"method": "initialized"})
            proxy.control = control_rpc
            proxy.selection = Selection(data, control_rpc, workers)
            return control_process, control_rpc

        await workers["first"].worker.rpc.close()
        assert (await workers["first"].worker.credentials()).account_id == "first"
        process, rpc = await start_control()
        first_config = await configure(rpc, int(router_url.rsplit(":", 1)[1]), auth_command)
        second_config = await configure(rpc, int(router_url.rsplit(":", 1)[1]), auth_command)
        assert first_config["changed"] and not second_config["changed"]
        moved_python = run / "relocated-python"
        moved_python.symlink_to(sys.executable)
        moved = await configure(
            rpc, int(router_url.rsplit(":", 1)[1]), dict(auth_command, command=str(moved_python))
        )
        restored = await configure(rpc, int(router_url.rsplit(":", 1)[1]), auth_command)
        assert moved["changed"] and restored["changed"]
        thread = await create_task(rpc, run, "Synthetic account-router task")
        await proxy.selection.select(thread, "first")
        first_turn = await begin_task(rpc, thread, "first synthetic turn")
        attached = await rpc.call("thread/read", {"threadId": thread, "includeTurns": False})
        assert attached["thread"]["id"] == thread
        assert attached["thread"]["modelProvider"] == "account-router"
        statuses = [(await rpc.completed(first_turn))["status"]]
        state["limit"] = True
        statuses.append((await rpc.turn(thread, "synthetic limit"))["status"])
        await proxy.selection.select(thread, "second")
        statuses.append((await rpc.turn(thread, "continue on second"))["status"])
        state["reject"] = True
        statuses.append((await rpc.turn(thread, "synthetic bad execution auth"))["status"])
        state["reject"] = False
        await close_stock(process, rpc)
        proxy.denied.clear()  # Reload the router's in-memory auth quarantine too.
        process, rpc = await start_control()
        resumed = await rpc.call("thread/resume", {"threadId": thread})
        assert resumed["modelProvider"] == "account-router"
        statuses.append((await rpc.turn(thread, "continue after restart"))["status"])
        assert statuses == ["completed", "failed", "completed", "failed", "completed"], statuses
        assert refreshes == ["second"], refreshes
        assert (control_home / "auth.json").read_bytes() == control_auth
        assert "first_OK" in json.dumps(requests[-1]["payload"])
        async with asyncio.timeout(5):
            while proxy.selection.active:
                await asyncio.sleep(0.02)
        result = {
            "synthetic_only": True,
            "maintained_router": True,
            "execution_worker_restart": True,
            "network": "stock processes restricted to localhost",
            "config_rpc_idempotent": True,
            "managed_auth_environment_relocation": True,
            "command_backed_caller_auth": True,
            "initial_prompt_allows_same_server_attachment": True,
            "turn_statuses": statuses,
            "control_credential_unchanged": True,
            "refreshes": refreshes,
            "execution_sequence": [row["account"] for row in requests],
            "same_thread_after_cold_resume": True,
            "final_status": await proxy.selection.status(),
        }
        (run / "result.json").write_text(json.dumps(result, indent=2) + "\n")
        print(json.dumps(result, indent=2))


if __name__ == "__main__":
    asyncio.run(main())
