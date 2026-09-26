import asyncio
import gzip
import json
import unittest

import aiohttp
from aiohttp import web
from aiohttp.test_utils import TestServer

from codex_account_router.accounts import Credentials
from codex_account_router.proxy import PREFIX, Proxy, application


class Control:
    def __init__(self):
        self.calls = []

    async def call(self, method, params):
        self.calls.append((method, params))
        return {"authMethod": "chatgpt", "authToken": "synthetic-control"}


class Worker:
    def __init__(self, origin):
        self.origin = origin
        self.refreshes = 0

    async def credentials(self, *, refresh=False, rejected_token=None):
        if refresh:
            assert rejected_token == f"synthetic-execution-{self.refreshes}"
        self.refreshes += int(refresh)
        return Credentials(f"synthetic-execution-{self.refreshes}", "execution", self.origin, "us")


class ProxyTest(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.requests = []
        self.response_status = 200
        self.reject_first = False
        self.hold = asyncio.Event()
        self.release = asyncio.Event()
        self.partial = False

        async def upstream(request):
            self.requests.append((await request.read(), dict(request.headers)))
            if self.reject_first and len(self.requests) == 1:
                return web.Response(status=401)
            if self.response_status != 200:
                return web.Response(status=self.response_status, body=b"synthetic error")
            response = web.StreamResponse(headers={"Content-Type": "text/event-stream"})
            await response.prepare(request)
            await response.write(b"data: first\n\n")
            self.hold.set()
            await self.release.wait()
            if self.partial:
                request.transport.abort()
            else:
                await response.write(b"data: last\n\n")
                await response.write_eof()
            return response

        backend = web.Application()
        backend.router.add_route("*", "/{path:.*}", upstream)
        self.backend = TestServer(backend)
        await self.backend.start_server(auto_decompress=False)
        self.session = aiohttp.ClientSession(auto_decompress=False)
        self.control = Control()
        self.worker = Worker(str(self.backend.make_url("")))
        self.proxy = Proxy(self.control, None, {"first": self.worker}, self.session)
        self.server = TestServer(application(self.proxy))
        await self.server.start_server(auto_decompress=False)
        self.headers = {"Authorization": "Bearer synthetic-control"}

    async def asyncTearDown(self):
        self.release.set()
        await self.server.close()
        await self.session.close()
        await self.backend.close()

    async def request(self, *, data=b""):
        return await self.session.get(
            self.server.make_url(PREFIX + "/models"), headers=self.headers, data=data
        )

    async def test_streams_before_completion_and_preserves_compressed_bytes(self):
        body = gzip.compress(b"synthetic compressed payload")
        self.headers.update(
            {
                "Content-Encoding": "gzip",
                "ChatGPT-Account-Id": "control",
                "x-openai-account-routing-override": "control-route",
                "Cookie": "control-cookie",
            }
        )
        async with await self.request(data=body) as response:
            self.assertEqual(
                await asyncio.wait_for(response.content.readline(), 2), b"data: first\n"
            )
            self.assertFalse(self.release.is_set())
            self.release.set()
            await response.read()
        received, headers = self.requests[0]
        self.assertEqual(
            (
                received,
                headers.get("Content-Encoding"),
                headers.get("Authorization"),
                headers.get("ChatGPT-Account-Id"),
                headers.get("x-openai-account-routing-override"),
                headers.get("Cookie"),
            ),
            (body, "gzip", "Bearer synthetic-execution-0", "execution", "us", None),
        )

    async def test_execution_401_refreshes_once_and_never_escapes_to_control(self):
        self.response_status = 401
        async with await self.request() as response:
            self.assertEqual(response.status, 403)
            self.assertIn("needs a new login", (await response.json())["error"]["message"])
        self.assertEqual((len(self.requests), self.worker.refreshes), (2, 1))
        async with await self.request() as retry:
            self.assertEqual(retry.status, 403)
        self.assertEqual((len(self.requests), self.worker.refreshes), (2, 1))
        self.assertTrue(all(not params["refreshToken"] for _, params in self.control.calls))

    async def test_model_turn_refreshes_then_streams_successfully(self):
        from contextlib import asynccontextmanager

        class Choices:
            def __init__(self):
                self.received = []

            @asynccontextmanager
            async def route(self, thread_id, turn_id):
                self.received.append((thread_id, turn_id))
                yield "first"

            async def receipt(self, thread_id, label, status):
                self.received.append((thread_id, label, status))

        choices = Choices()
        self.proxy.selection = choices
        self.reject_first = True
        self.release.set()
        self.headers.update(
            {
                "thread-id": "task",
                "x-codex-turn-metadata": json.dumps({"thread_id": "task", "turn_id": "turn"}),
            }
        )
        async with self.session.post(
            self.server.make_url(PREFIX + "/responses"), headers=self.headers, data=b"payload"
        ) as response:
            self.assertEqual(
                (response.status, await response.read()), (200, b"data: first\n\ndata: last\n\n")
            )
        self.assertEqual(
            (len(self.requests), self.worker.refreshes, choices.received),
            (2, 1, [("task", "turn"), ("task", "first", 200)]),
        )

    async def test_limit_is_preserved_and_rejected_callers_never_reach_execution(self):
        self.response_status = 429
        async with await self.request() as response:
            self.assertEqual((response.status, await response.read()), (429, b"synthetic error"))
        self.headers["Authorization"] = "Bearer wrong"
        async with await self.request() as response:
            self.assertEqual(response.status, 403)
        self.headers["Authorization"] = "Bearer synthetic-control"
        self.headers["Origin"] = "https://unrelated.example"
        async with await self.request() as response:
            self.assertEqual(response.status, 403)
        self.assertEqual((len(self.requests), self.worker.refreshes), (1, 0))

    async def test_partial_response_is_not_replayed(self):
        self.partial = True
        async with await self.request() as response:
            await asyncio.wait_for(response.content.readline(), 2)
            self.release.set()
            with self.assertRaises(aiohttp.ClientPayloadError):
                await response.read()
        self.assertEqual(len(self.requests), 1)

    async def test_affinity_header_cannot_replace_actual_thread_identity(self):
        self.headers.update(
            {
                "session-id": "affinity-only",
                "thread-id": "another-thread",
                "x-codex-turn-metadata": json.dumps({"thread_id": "actual", "turn_id": "turn"}),
            }
        )
        async with self.session.post(
            self.server.make_url(PREFIX + "/responses"), headers=self.headers
        ) as response:
            self.assertEqual(response.status, 403)
        self.assertEqual(self.requests, [])
        self.headers["x-codex-turn-metadata"] = "[]"
        async with self.session.post(
            self.server.make_url(PREFIX + "/responses"), headers=self.headers
        ) as malformed:
            self.assertEqual(malformed.status, 403)
