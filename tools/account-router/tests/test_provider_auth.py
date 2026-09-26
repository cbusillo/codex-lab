import asyncio
import json
import sys
import tempfile
import unittest
from pathlib import Path

from aiohttp import web


class ProviderAuthTest(unittest.IsolatedAsyncioTestCase):
    async def test_command_exports_only_current_control_token_without_forced_refresh(self):
        current = {"authMethod": "chatgpt", "authToken": "synthetic-first"}
        calls = []

        async def handle(request):
            websocket = web.WebSocketResponse()
            await websocket.prepare(request)
            async for message in websocket:
                request_data = json.loads(message.data)
                if request_data.get("method") == "initialized":
                    continue
                if request_data["method"] == "initialize":
                    result = {"userAgent": "codex/0.157.1"}
                else:
                    calls.append((request_data["method"], request_data["params"]))
                    result = dict(current)
                await websocket.send_json({"id": request_data["id"], "result": result})
            return websocket

        app = web.Application()
        app.router.add_get("/", handle)
        runner = web.AppRunner(app)
        await runner.setup()
        try:
            with tempfile.TemporaryDirectory() as directory:
                socket = Path(directory) / "auth.sock"
                await web.UnixSite(runner, str(socket)).start()
                for token in ("synthetic-first", "synthetic-rotated", None):
                    current["authToken"] = token
                    process = await asyncio.create_subprocess_exec(
                        sys.executable,
                        "-m",
                        "codex_account_router.provider_auth",
                        str(socket),
                        stdout=asyncio.subprocess.PIPE,
                        stderr=asyncio.subprocess.PIPE,
                    )
                    output, error = await asyncio.wait_for(process.communicate(), 10)
                    if token is not None:
                        self.assertEqual(
                            (process.returncode, output, error), (0, f"{token}\n".encode(), b"")
                        )
                    else:
                        self.assertEqual(
                            (process.returncode, output, error),
                            (1, b"", b"control credential lookup failed\n"),
                        )
            self.assertEqual(
                calls, [("getAuthStatus", {"includeToken": True, "refreshToken": False})] * 3
            )
        finally:
            await runner.cleanup()
