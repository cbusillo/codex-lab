import asyncio
import base64
import json
import sys
import tempfile
import unittest
from pathlib import Path

from codex_account_router.accounts import (
    AccountError,
    AccountWorker,
    Credentials,
    Lease,
    account_home,
    credentials_from,
    read_metadata,
)


def token(identity, revision="old"):
    claims = {"https://api.openai.com/auth": {"chatgpt_account_id": identity}, "revision": revision}
    encoded = base64.urlsafe_b64encode(json.dumps(claims).encode()).decode().rstrip("=")
    return f"header.{encoded}.synthetic"


def routing(identity):
    return {
        "workspaceRouting": {
            "chatgptAccountId": identity,
            "backendOrigin": "https://chatgpt.com",
            "accountRoutingOverride": "NO_CONSTRAINT",
        }
    }


class FakeRpc:
    def __init__(self):
        self.identity = "account-a"
        self.revision = "old"
        self.refreshes = 0
        self.rotate = True

    async def call(self, method, params):
        await asyncio.sleep(0)
        if method == "account/read":
            return routing(self.identity)
        if params["refreshToken"]:
            self.refreshes += 1
            if self.rotate:
                self.revision = "new"
        return {"authMethod": "chatgpt", "authToken": token(self.identity, self.revision)}

    async def close(self):
        pass


class AccountsTest(unittest.IsolatedAsyncioTestCase):
    async def test_control_or_duplicate_execution_identity_is_rejected_before_binding(self):
        with tempfile.TemporaryDirectory() as directory:
            home = account_home(Path(directory), "first")
            worker = AccountWorker(
                home, Lease(home / "worker.lock"), FakeRpc(), excluded_ids={"account-a"}
            )
            try:
                with self.assertRaisesRegex(AccountError, "must differ"):
                    await worker.credentials()
                self.assertFalse((home / "router-identity.json").exists())
                damaged = home / "selection.json"
                damaged.write_text('{"partial":')
                with self.assertRaisesRegex(AccountError, "preserve it for repair"):
                    read_metadata(damaged)
            finally:
                await worker.close()

    async def test_worker_inherited_lock_survives_parent_handle_closure(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "worker.lock"
            lease = Lease(path)
            child = await asyncio.create_subprocess_exec(
                sys.executable,
                "-c",
                "import sys; print('ready', flush=True); sys.stdin.read()",
                pass_fds=(lease.fd,),
                stdin=asyncio.subprocess.PIPE,
                stdout=asyncio.subprocess.PIPE,
            )
            input_stream, output_stream = child.stdin, child.stdout
            assert input_stream is not None and output_stream is not None
            try:
                await output_stream.readline()
                lease.close()
                with self.assertRaises(AccountError):
                    Lease(path)
            finally:
                lease.close()
                input_stream.close()
                await child.wait()
            Lease(path).close()

    async def test_unchanged_token_after_failed_refresh_is_not_refreshed_repeatedly(self):
        with tempfile.TemporaryDirectory() as directory:
            home = account_home(Path(directory), "first")
            rpc = FakeRpc()
            rpc.rotate = False
            worker = AccountWorker(home, Lease(home / "worker.lock"), rpc)
            try:
                old = await worker.credentials()
                for _ in range(3):
                    self.assertEqual(
                        await worker.credentials(refresh=True, rejected_token=old.token), old
                    )
                self.assertEqual(rpc.refreshes, 1)
            finally:
                await worker.close()

    async def test_concurrent_rejections_share_refresh_and_bind_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            home = account_home(Path(directory), "first")
            rpc = FakeRpc()
            worker = AccountWorker(home, Lease(home / "worker.lock"), rpc)
            try:
                old = await worker.credentials()
                received = await asyncio.gather(
                    *[worker.credentials(refresh=True, rejected_token=old.token) for _ in range(3)]
                )
                expected = Credentials(
                    token("account-a", "new"), "account-a", "https://chatgpt.com", "NO_CONSTRAINT"
                )
                self.assertEqual((received, rpc.refreshes), ([expected] * 3, 1))
                rpc.identity = "account-b"
                with self.assertRaisesRegex(AccountError, "changed identity"):
                    await worker.credentials()
                self.assertEqual(
                    json.loads((home / "router-identity.json").read_text()),
                    {"account_id": "account-a"},
                )
                self.assertNotIn("account-a", repr(old))
                self.assertNotIn(old.token, repr(old))
            finally:
                await worker.close()

    async def test_enrollment_and_worker_cannot_hold_same_account(self):
        with tempfile.TemporaryDirectory() as directory:
            home = account_home(Path(directory), "first")
            worker_lease = Lease(home / "worker.lock")
            try:
                with self.assertRaises(AccountError):
                    Lease(home / "worker.lock")
            finally:
                worker_lease.close()
            enrollment_lease = Lease(home / "worker.lock")
            enrollment_lease.close()
            with self.assertRaises(AccountError):
                account_home(Path(directory), "../control")

    async def test_credentials_reject_mismatched_identity_and_untrusted_destination(self):
        exported = {"authMethod": "chatgpt", "authToken": token("account-a")}
        bad = routing("account-b")
        with self.assertRaises(AccountError):
            credentials_from(exported, bad)
        bad = routing("account-a")
        bad["workspaceRouting"]["backendOrigin"] = "https://unrelated.example"
        with self.assertRaises(AccountError):
            credentials_from(exported, bad)
