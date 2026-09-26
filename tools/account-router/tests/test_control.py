import asyncio
import tempfile
import unittest
from pathlib import Path

from aiohttp import web

from codex_account_router.accounts import AccountError
from codex_account_router.control import Control
from codex_account_router.rpc import RpcError


class ControlTest(unittest.IsolatedAsyncioTestCase):
    async def test_reconnects_same_owner_but_refuses_changed_identity_and_mutations(self):
        owner = "control"
        sockets = []

        async def endpoint(request):
            socket = web.WebSocketResponse()
            await socket.prepare(request)
            sockets.append(socket)
            async for message in socket:
                item = message.json()
                if "id" not in item:
                    continue
                method = item["method"]
                result = (
                    {"userAgent": "stock/0.157.1 test"}
                    if method == "initialize"
                    else {"workspaceRouting": {"chatgptAccountId": owner}}
                )
                await socket.send_json({"id": item["id"], "result": result})
            return socket

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "control.sock"
            app = web.Application()
            app.router.add_get("/", endpoint)
            server = web.AppRunner(app)
            await server.setup()
            await web.UnixSite(server, str(path)).start()
            control = Control(path)
            try:
                expected = {"workspaceRouting": {"chatgptAccountId": "control"}}
                self.assertEqual(await control.call("account/read", {}), expected)
                await sockets[0].close()
                await asyncio.sleep(0)
                self.assertEqual(await control.call("account/read", {}), expected)
                self.assertEqual(len(sockets), 2)
                with self.assertRaises(RpcError):
                    await control.call("thread/start", {})
                owner = "different-owner"
                await sockets[1].close()
                await asyncio.sleep(0)
                with self.assertRaisesRegex(AccountError, "identity changed"):
                    await control.call("account/read", {})
            finally:
                await control.close()
                await server.cleanup()
