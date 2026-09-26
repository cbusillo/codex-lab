import asyncio
import signal
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from unittest.mock import AsyncMock, patch

import aiohttp
from test_accounts import FakeRpc

from codex_account_router.accounts import AccountError, AccountWorker, account_home
from codex_account_router.control import Control
from codex_account_router.server import serve


class ServerTest(unittest.IsolatedAsyncioTestCase):
    async def test_control_account_can_be_an_independently_enrolled_execution_choice(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            account_home(root, "phone-owner").joinpath("auth.json").write_text("{}")
            control_rpc, execution_rpc = FakeRpc(), FakeRpc()
            control = Control(root / "host.sock", connect=AsyncMock(return_value=control_rpc))
            args = Namespace(
                data_dir=root,
                control_socket=control.socket_path,
                codex="synthetic-unused",
                accounts=["phone-owner"],
                port=0,
            )
            handlers = {}
            loop = asyncio.get_running_loop()
            with (
                patch("codex_account_router.server.Control", return_value=control),
                patch.object(AccountWorker, "connect", return_value=execution_rpc),
                patch.object(loop, "add_signal_handler", side_effect=handlers.__setitem__),
                patch.object(loop, "remove_signal_handler", side_effect=handlers.pop),
            ):
                task = asyncio.create_task(serve(args))
                try:
                    async with asyncio.timeout(5):
                        while signal.SIGTERM not in handlers:
                            if task.done():
                                await task
                            await asyncio.sleep(0.01)
                    connector = aiohttp.UnixConnector(path=str(root / "control.sock"))
                    async with aiohttp.ClientSession(connector=connector) as client:
                        async with client.get("http://localhost/status") as response:
                            self.assertEqual(
                                (response.status, await response.json()),
                                (
                                    200,
                                    {
                                        "accounts": ["phone-owner"],
                                        "accountNames": {"phone-owner": "owner@example.invalid"},
                                        "tasks": [],
                                    },
                                ),
                            )
                    with self.assertRaisesRegex(AccountError, "already in use"):
                        await AccountWorker.start(root, "phone-owner", args.codex)
                    self.assertEqual((control_rpc.refreshes, execution_rpc.refreshes), (0, 0))
                finally:
                    if signal.SIGTERM in handlers:
                        handlers[signal.SIGTERM]()
                    else:
                        task.cancel()
                    await asyncio.gather(task, return_exceptions=True)
                self.assertFalse((root / "control.sock").exists())
