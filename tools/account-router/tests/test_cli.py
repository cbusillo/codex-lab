import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from codex_account_router.accounts import AccountError
from codex_account_router.cli import begin_task, run
from codex_account_router.rpc import ConnectionLost, RpcError


class LauncherTest(unittest.IsolatedAsyncioTestCase):
    async def test_connection_loss_after_submission_reports_how_to_reconnect(self):
        rpc = SimpleNamespace(
            call=AsyncMock(side_effect=[{"turn": {"id": "turn"}}, ConnectionLost("closed")])
        )
        with self.assertRaisesRegex(AccountError, "may still be running.*resume task"):
            await begin_task(rpc, "task", "owner request")
        self.assertEqual(rpc.call.await_count, 2)

    async def test_resume_uses_the_socket_owners_config_and_never_starts_a_turn(self):
        rpc = SimpleNamespace(
            call=AsyncMock(
                side_effect=[
                    {
                        "thread": {
                            "id": "task",
                            "modelProvider": "account-router",
                            "cwd": "/project",
                        }
                    },
                    {"layers": [{"name": {"type": "user", "file": "/control-home/config.toml"}}]},
                ]
            ),
            close=AsyncMock(),
        )
        args = SimpleNamespace(
            command="resume", thread="task", control_socket=Path("/stock.sock"), codex="stock"
        )
        process = SimpleNamespace(returncode=0, wait=AsyncMock(return_value=0))
        with (
            patch("codex_account_router.cli.Rpc.local", AsyncMock(return_value=rpc)),
            patch("codex_account_router.cli.admin", AsyncMock(return_value={"accounts": ["a"]})),
            patch("codex_account_router.cli.os.environ", {"CODEX_HOME": "/different-home"}),
            patch(
                "codex_account_router.cli.asyncio.create_subprocess_exec",
                AsyncMock(return_value=process),
            ) as spawn,
        ):
            result = await run(args)
        self.assertEqual(
            (result, spawn.call_args.kwargs, [call.args[0] for call in rpc.call.call_args_list]),
            (
                {"thread": "task"},
                {"cwd": Path("/project"), "env": {"CODEX_HOME": "/control-home"}},
                ["thread/read", "config/read"],
            ),
        )

    async def test_resume_rejects_an_unrelated_provider_without_starting_a_client(self):
        rpc = SimpleNamespace(
            call=AsyncMock(return_value={"thread": {"id": "task", "modelProvider": "openai"}}),
            close=AsyncMock(),
        )
        args = SimpleNamespace(command="resume", thread="task", control_socket=Path("/stock.sock"))
        with (
            patch("codex_account_router.cli.Rpc.local", AsyncMock(return_value=rpc)),
            patch("codex_account_router.cli.admin", AsyncMock(return_value={"accounts": ["a"]})),
            patch("codex_account_router.cli.asyncio.create_subprocess_exec", AsyncMock()) as spawn,
        ):
            with self.assertRaisesRegex(AccountError, "does not use"):
                await run(args)
        spawn.assert_not_called()
        rpc.close.assert_awaited_once()

    async def test_delayed_history_does_not_replay_the_first_user_turn(self):
        calls = []

        class Stock:
            async def call(self, method, params):
                calls.append((method, params))
                if method == "turn/start":
                    return {"turn": {"id": "first-turn"}}
                if len(calls) < 4:
                    raise RpcError("history is not persisted yet")
                return {"thread": {"id": "same-task"}}

        result = await begin_task(Stock(), "same-task", "the owner's first request")
        self.assertEqual(
            (result, calls),
            (
                "first-turn",
                [
                    (
                        "turn/start",
                        {
                            "threadId": "same-task",
                            "input": [{"type": "text", "text": "the owner's first request"}],
                        },
                    ),
                    *[("thread/resume", {"threadId": "same-task", "excludeTurns": True})] * 3,
                ],
            ),
        )
