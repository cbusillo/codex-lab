import io
import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock, call, patch

from codex_account_router.accounts import AccountError
from codex_account_router.cli import run


class PhoneTest(unittest.IsolatedAsyncioTestCase):
    async def test_choice_round_trip_keeps_shell_metacharacters_as_data(self):
        account_names = {
            "execution-a": "first@example.invalid",
            "execution-b": "second@example.invalid",
            "phone-owner": "owner@example.invalid",
        }
        status = {
            "accounts": ["execution-a", "execution-b", "phone-owner"],
            "accountNames": account_names,
            "tasks": [
                {
                    "thread": "task-id",
                    "name": "Review\n$(touch /tmp/no) | 'quotes'",
                    "state": "idle",
                },
                {"thread": "busy-id", "name": "Busy", "state": "active"},
            ],
        }
        args = SimpleNamespace(command="phone-list")
        admin = AsyncMock(side_effect=[status, status, {"execution": "execution-b"}])
        with patch("codex_account_router.cli.admin", admin):
            listed = await run(args)
            self.assertEqual(
                listed.splitlines(),
                [
                    f"Review $(touch /tmp/no) | 'quotes' → {email} ({label}) · task-id"
                    for label, email in account_names.items()
                ],
            )
            selected = listed.splitlines()[1]
            args.command = "phone-select"
            with patch("sys.stdin", io.StringIO(selected + "\n")):
                result = await run(args)
        self.assertEqual(
            result,
            "Selected second@example.invalid (execution-b) for the next turn.\n"
            "Your phone login stays unchanged.",
        )
        self.assertEqual(
            admin.call_args_list[-1],
            call(args, "POST", "/select", {"thread": "task-id", "execution": "execution-b"}),
        )

    async def test_stale_or_busy_choice_never_changes_selection(self):
        args = SimpleNamespace(command="phone-select")
        admin = AsyncMock(return_value={"accounts": ["a"], "tasks": []})
        with (
            patch("codex_account_router.cli.admin", admin),
            patch("sys.stdin", io.StringIO("old choice")),
        ):
            with self.assertRaisesRegex(AccountError, "unavailable"):
                await run(args)
        self.assertEqual(admin.call_args_list, [call(args, "GET", "/status")])

    async def test_empty_inventory_stops_the_shortcut_before_selection(self):
        args = SimpleNamespace(command="phone-list")
        with patch(
            "codex_account_router.cli.admin",
            AsyncMock(return_value={"accounts": ["a"], "tasks": []}),
        ):
            with self.assertRaisesRegex(AccountError, "no idle"):
                await run(args)
