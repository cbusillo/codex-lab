import asyncio
import json
import unittest

from codex_account_router.rpc import Rpc, RpcError


class RpcTest(unittest.IsolatedAsyncioTestCase):
    async def test_server_request_id_does_not_complete_client_call(self):
        sent = asyncio.Queue()

        async def incoming():
            item = await sent.get()
            yield json.dumps({"id": item["id"], "method": "item/commandExecution/requestApproval"})
            yield json.dumps({"id": item["id"], "result": {"correct_response": True}})

        async def close():
            pass

        rpc = Rpc(sent.put, incoming(), close)
        try:
            self.assertEqual(await rpc.call("account/read", {}), {"correct_response": True})
        finally:
            await rpc.close()

    async def test_eof_fails_pending_call_without_waiting_for_timeout(self):
        incoming = asyncio.StreamReader()

        async def send(_item):
            incoming.feed_eof()

        async def close():
            pass

        rpc = Rpc(send, incoming, close)
        try:
            with self.assertRaisesRegex(RpcError, "connection closed"):
                await asyncio.wait_for(rpc.call("account/read", {}), 1)
        finally:
            await rpc.close()
