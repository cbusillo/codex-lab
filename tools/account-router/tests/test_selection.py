import asyncio
import tempfile
import unittest
from pathlib import Path

from codex_account_router.accounts import AccountError
from codex_account_router.selection import Selection


class Host:
    def __init__(self):
        self.threads = {}

    def add(self, identity, parent=None):
        self.threads[identity] = {
            "id": identity,
            "modelProvider": "account-router",
            "status": {"type": "idle"},
            "preview": identity,
            "parentThreadId": parent,
        }

    async def call(self, method, params):
        assert method == "thread/read"
        return {"thread": self.threads[params["threadId"]]}


class SelectionTest(unittest.IsolatedAsyncioTestCase):
    async def test_cancellation_clears_active_count_even_when_selection_lock_is_held(self):
        with tempfile.TemporaryDirectory() as directory:
            host = Host()
            host.add("task")
            choices = Selection(Path(directory), host, ["first", "second"])
            await choices.select("task", "first")
            started = asyncio.Event()

            async def request():
                async with choices.route("task", "turn"):
                    started.set()
                    await asyncio.Event().wait()

            task = asyncio.create_task(request())
            await started.wait()
            lock = choices.lock("task")
            await lock.acquire()
            try:
                task.cancel()
                with self.assertRaises(asyncio.CancelledError):
                    await asyncio.wait_for(task, 1)
                self.assertEqual(choices.active, {})
            finally:
                lock.release()
            await choices.select("task", "second")

    async def test_phone_listing_bounds_registered_tasks_and_omits_inherited_children(self):
        with tempfile.TemporaryDirectory() as directory:
            host = Host()
            choices = Selection(Path(directory), host, ["first"])
            for index in range(102):
                thread_id = f"task-{index}"
                host.add(thread_id)
                await choices.select(thread_id, "first")
            host.add("child", "task-101")
            async with choices.route("child", "child-turn"):
                pass
            result = await choices.status()
            self.assertEqual(
                [row["thread"] for row in result["tasks"]],
                [f"task-{index}" for index in range(2, 102)],
            )

    async def test_descendant_resolution_cannot_overwrite_a_concurrent_parent_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            host = Host()
            host.add("root")
            host.add("parent", "root")
            host.add("leaf", "parent")
            choices = Selection(Path(directory), host, ["first", "second"])
            await choices.select("root", "first")
            async with choices.route("parent", "old-turn"):
                pass
            await choices.select("root", "second")
            original = host.call
            waiting, release = asyncio.Event(), asyncio.Event()

            async def delayed(method, params):
                current_task = asyncio.current_task()
                assert current_task is not None
                if params["threadId"] == "root" and current_task.get_name() == "leaf-routing":
                    waiting.set()
                    await release.wait()
                return await original(method, params)

            async def leaf_request():
                async with choices.route("leaf", "leaf-turn"):
                    pass

            host.call = delayed
            leaf = asyncio.create_task(leaf_request(), name="leaf-routing")
            await waiting.wait()
            async with choices.route("parent", "new-turn") as account:
                release.set()
                await leaf
                self.assertEqual(
                    (
                        account,
                        choices.entries["parent"]["pinTurn"],
                        choices.entries["parent"]["pinLabel"],
                    ),
                    ("second", "new-turn", "second"),
                )

    async def test_existing_child_inherits_new_parent_choice_on_its_next_turn(self):
        with tempfile.TemporaryDirectory() as directory:
            host = Host()
            host.add("parent")
            host.add("child", "parent")
            choices = Selection(Path(directory), host, ["first", "second"])
            await choices.select("parent", "first")
            async with choices.route("parent", "parent-one"):
                async with choices.route("child", "child-one") as original:
                    self.assertEqual(original, "first")
            await choices.select("parent", "second")
            async with choices.route("child", "child-one") as pinned:
                self.assertEqual(pinned, "first")
            async with choices.route("child", "child-two") as inherited:
                self.assertEqual(inherited, "second")
            await choices.select("child", "first")
            async with choices.route("child", "child-three") as explicit:
                self.assertEqual(explicit, "first")

    async def test_slow_status_does_not_block_other_task_routing(self):
        with tempfile.TemporaryDirectory() as directory:
            host = Host()
            host.add("slow")
            host.add("ready")
            choices = Selection(Path(directory), host, ["first"])
            await choices.select("slow", "first")
            await choices.select("ready", "first")
            original = host.call
            release, started = asyncio.Event(), asyncio.Event()

            async def delayed(method, params):
                if params["threadId"] == "slow":
                    started.set()
                    await release.wait()
                return await original(method, params)

            host.call = delayed
            status = asyncio.create_task(choices.status())
            await started.wait()
            try:
                async with asyncio.timeout(1), choices.route("ready", "turn") as label:
                    self.assertEqual(label, "first")
            finally:
                release.set()
                await status

    async def test_pin_survives_restart_and_idle_switch_affects_next_turn_only(self):
        with tempfile.TemporaryDirectory() as directory:
            host = Host()
            host.add("task")
            root = Path(directory)
            choices = Selection(root, host, ["first", "second"])
            await choices.select("task", "first")
            async with choices.route("task", "turn-one") as first:
                with self.assertRaisesRegex(AccountError, "busy"):
                    await choices.select("task", "second")
            host.threads["task"]["status"] = {"type": "systemError"}
            await choices.select("task", "second")
            choices = Selection(root, host, ["first", "second"])
            async with choices.route("task", "turn-one") as retry:
                self.assertEqual((first, retry), ("first", "first"))
            async with choices.route("task", "turn-two") as second:
                self.assertEqual(second, "second")
            host.threads["task"]["status"] = {"type": "active"}
            with self.assertRaisesRegex(AccountError, "busy"):
                await choices.select("task", "first")

    async def test_only_verified_children_inherit_and_other_tasks_keep_their_choice(self):
        with tempfile.TemporaryDirectory() as directory:
            host = Host()
            for identity, parent in [
                ("parent", None),
                ("child", "parent"),
                ("other", None),
                ("unknown", None),
            ]:
                host.add(identity, parent)
            choices = Selection(Path(directory), host, ["first", "second"])
            await choices.select("parent", "first")
            await choices.select("other", "second")
            async with choices.route("parent", "parent-turn"):
                async with choices.route("child", "child-turn") as child:
                    async with choices.route("other", "other-turn") as other:
                        self.assertEqual((child, other), ("first", "second"))
                with self.assertRaisesRegex(AccountError, "no execution selection"):
                    async with choices.route("unknown", "unknown-turn"):
                        self.fail("unselected task accepted")
            host.threads["other"]["modelProvider"] = "openai"
            with self.assertRaisesRegex(AccountError, "does not use"):
                await choices.select("other", "first")
