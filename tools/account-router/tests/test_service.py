import asyncio
import json
import logging
import os
import plistlib
import sys
import tempfile
import unittest
from dataclasses import replace
from pathlib import Path
from unittest.mock import AsyncMock, patch

from codex_account_router.accounts import AccountError, Lease
from codex_account_router.service import (
    Service,
    TaskObserver,
    launch_agent,
    refresh_subscriptions,
    supervise,
)


class ServiceTest(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name).resolve()
        self.home = self.root / "phone"
        self.home.mkdir()
        self.home.joinpath("auth.json").write_text("synthetic; never parsed")
        self.binary = self.root / "fake-codex"
        self.binary.write_text(f"#!{sys.executable}\nimport time\ntime.sleep(60)\n")
        self.binary.chmod(0o700)
        self.service = Service(str(self.binary), self.root, self.home, "control-id", ["first"])
        self.stopped = asyncio.Event()
        self.ready = asyncio.Event()
        self.processes = []
        spawn = asyncio.create_subprocess_exec

        async def record_process(*args, **kwargs):
            process = await spawn(*args, **kwargs)
            self.processes.append(process)
            return process

        self.spawn_patch = patch(
            "codex_account_router.service.asyncio.create_subprocess_exec", record_process
        )
        self.spawn_patch.start()
        self.addCleanup(self.spawn_patch.stop)

    async def serve(self, _service, *, stopped, **_options):
        self.ready.set()
        await stopped.wait()

    async def test_attached_service_never_owns_the_existing_host(self):
        service = replace(self.service, mode="attach")
        rpc = AsyncMock()
        rpc.call.return_value = {"workspaceRouting": {"chatgptAccountId": "control-id"}}
        self.home.joinpath("auth.json").unlink()
        with (
            patch("codex_account_router.service.Rpc.local", return_value=rpc),
            patch("codex_account_router.service.serve", self.serve),
        ):
            task = asyncio.create_task(supervise(service, self.stopped, logging.getLogger()))
            try:
                await asyncio.wait_for(self.ready.wait(), 5)
            finally:
                self.stopped.set()
                await asyncio.wait_for(task, 5)
        self.assertEqual(self.processes, [])
        rpc.close.assert_awaited_once()
        self.assertFalse((self.home / "owner.lock").exists())

    async def test_attached_wrong_identity_does_not_touch_host_or_enable_remote(self):
        rpc = AsyncMock()
        rpc.call.return_value = {"workspaceRouting": {"chatgptAccountId": "another-id"}}
        with patch("codex_account_router.service.Rpc.local", return_value=rpc):
            with self.assertRaisesRegex(AccountError, "control identity changed"):
                await supervise(
                    replace(self.service, mode="attach"), self.stopped, logging.getLogger()
                )
        self.assertEqual(self.processes, [])
        rpc.call.assert_awaited_once_with("account/read", {"refreshToken": False})

    async def test_adoption_keeps_the_same_task_subscribed_without_starting_a_turn(self):
        rpc = AsyncMock()
        rpc.call.side_effect = [
            {"thread": {"id": "existing", "status": {"type": "notLoaded"}}},
            {"thread": {"id": "existing"}, "modelProvider": "account-router"},
        ]
        selection = AsyncMock(labels={"first"})

        async def select(thread, label):
            (self.root / "selection.json").write_text(json.dumps({thread: {"label": label}}))
            return {"thread": thread, "execution": label}

        selection.select.side_effect = select
        observer = TaskObserver(rpc, self.root)
        result = await observer.adopt("existing", "first", selection)
        self.assertEqual(
            (result, observer.subscribed, [call.args[0] for call in rpc.call.call_args_list]),
            (
                {"thread": "existing", "execution": "first"},
                {"existing"},
                ["thread/read", "thread/resume"],
            ),
        )

    async def test_busy_or_attached_tasks_cannot_be_adopted(self):
        for state, provider, expected in (
            ("active", "openai", "busy"),
            ("idle", "openai", "still attached"),
        ):
            with self.subTest(state=state):
                rpc = AsyncMock()
                rpc.call.side_effect = [
                    {"thread": {"id": "existing", "status": {"type": state}}},
                    {"thread": {"id": "existing"}, "modelProvider": provider},
                    {},
                ]
                selection = AsyncMock(labels={"first"})
                observer = TaskObserver(rpc, self.root)
                with self.assertRaisesRegex(AccountError, expected):
                    await observer.adopt("existing", "first", selection)
                selection.select.assert_not_awaited()
                self.assertEqual(observer.subscribed, set())
                if state == "idle":
                    self.assertEqual(
                        rpc.call.call_args,
                        unittest.mock.call("thread/unsubscribe", {"threadId": "existing"}),
                    )

    async def test_owned_startup_and_shutdown_preserve_home(self):
        rpc = AsyncMock()
        rpc.call.return_value = {"workspaceRouting": {"chatgptAccountId": "control-id"}}
        with (
            patch("codex_account_router.service.Rpc.local", return_value=rpc),
            patch("codex_account_router.service.serve", self.serve),
        ):
            task = asyncio.create_task(supervise(self.service, self.stopped, logging.getLogger()))
            try:
                await asyncio.wait_for(self.ready.wait(), 5)
                with self.assertRaisesRegex(AccountError, "already in use"):
                    await supervise(self.service, asyncio.Event(), logging.getLogger())
                self.assertEqual(
                    rpc.call.call_args_list,
                    [
                        unittest.mock.call("account/read", {"refreshToken": False}),
                        unittest.mock.call("remoteControl/enable", {"ephemeral": True}),
                    ],
                )
            finally:
                self.stopped.set()
                await asyncio.wait_for(task, 5)
        self.assertIsNotNone(self.processes[0].returncode)
        Lease(self.home / "owner.lock").close()
        self.assertEqual(self.home.joinpath("auth.json").read_text(), "synthetic; never parsed")

    async def test_wrong_owner_stops_host_before_remote_or_router(self):
        rpc = AsyncMock()
        rpc.call.return_value = {"workspaceRouting": {"chatgptAccountId": "another-id"}}
        with (
            patch("codex_account_router.service.Rpc.local", return_value=rpc),
            patch("codex_account_router.service.serve", self.serve),
        ):
            with self.assertRaisesRegex(AccountError, "control identity changed"):
                await supervise(self.service, self.stopped, logging.getLogger())
        self.assertEqual(rpc.call.await_count, 1)
        self.assertFalse(self.ready.is_set())
        self.assertIsNotNone(self.processes[0].returncode)

    async def test_host_failure_stops_router_and_releases_ownership(self):
        rpc = AsyncMock()
        rpc.call.return_value = {"workspaceRouting": {"chatgptAccountId": "control-id"}}
        with (
            patch("codex_account_router.service.Rpc.local", return_value=rpc),
            patch("codex_account_router.service.serve", self.serve),
        ):
            task = asyncio.create_task(supervise(self.service, self.stopped, logging.getLogger()))
            try:
                await asyncio.wait_for(self.ready.wait(), 5)
                self.processes[0].terminate()
                with self.assertRaisesRegex(AccountError, "component exited"):
                    await asyncio.wait_for(task, 5)
            finally:
                self.stopped.set()
                await asyncio.gather(task, return_exceptions=True)
        Lease(self.home / "owner.lock").close()

    async def test_stop_during_startup_leaves_no_stock_child(self):
        with patch("codex_account_router.service.Rpc.local", side_effect=ConnectionRefusedError):
            task = asyncio.create_task(supervise(self.service, self.stopped, logging.getLogger()))
            try:
                async with asyncio.timeout(5):
                    while not self.processes:
                        await asyncio.sleep(0.01)
            finally:
                self.stopped.set()
                await asyncio.wait_for(task, 5)
        self.assertIsNotNone(self.processes[0].returncode)

    async def test_live_unmanaged_socket_is_never_replaced(self):
        server = await asyncio.start_unix_server(
            lambda _r, w: w.close(), self.service.control_socket
        )
        try:
            with self.assertRaisesRegex(AccountError, "already serving"):
                await supervise(self.service, self.stopped, logging.getLogger())
            self.assertEqual(self.processes, [])
        finally:
            server.close()
            await server.wait_closed()

    async def test_registered_tasks_are_kept_loaded_without_starting_turns(self):
        registry = self.root / "selection.json"
        registry.write_text(json.dumps({"existing": {"label": "first"}}))
        rpc = AsyncMock()
        rpc.call.return_value = {"modelProvider": "account-router"}
        subscribed = set()
        await refresh_subscriptions(rpc, self.root, subscribed)
        registry.write_text(
            json.dumps({"next": {"label": "first"}, "child": {"inheritedFrom": "next"}})
        )
        await refresh_subscriptions(rpc, self.root, subscribed)
        self.assertEqual(subscribed, {"next"})
        self.assertEqual(
            rpc.call.call_args_list,
            [
                unittest.mock.call(
                    "thread/resume",
                    {
                        "threadId": "existing",
                        "modelProvider": "account-router",
                        "excludeTurns": True,
                    },
                ),
                unittest.mock.call(
                    "thread/resume",
                    {"threadId": "next", "modelProvider": "account-router", "excludeTurns": True},
                ),
                unittest.mock.call("thread/unsubscribe", {"threadId": "existing"}),
            ],
        )

    def test_rendered_agent_loads_explicit_configuration_outside_the_source_tree(self):
        config = self.root / "service.json"
        config.write_text(
            json.dumps(
                {
                    "codex": str(self.binary),
                    "data_dir": str(self.root),
                    "control_home": str(self.home),
                    "control_account_id": "control-id",
                    "accounts": ["first"],
                }
            )
        )
        service = Service.load(config)
        agent = plistlib.loads(launch_agent(config, service).encode())
        self.assertEqual(Service.load(Path(agent["ProgramArguments"][-1])), service)
        self.assertEqual(Path(agent["WorkingDirectory"]), service.data_dir)
        self.assertTrue(os.path.isabs(agent["ProgramArguments"][0]))
        config.write_text(config.read_text().replace(str(self.home), str(self.root.parent)))
        with self.assertRaises(AccountError):
            Service.load(config)
        values = json.loads(config.read_text())
        values["mode"] = "attach"
        config.write_text(json.dumps(values))
        attached = Service.load(config)
        self.assertEqual(
            attached.control_socket,
            self.root.parent / "app-server-control" / "app-server-control.sock",
        )
