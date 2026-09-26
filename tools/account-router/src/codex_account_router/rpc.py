"""Bounded owner-local app-server RPC, without printing protocol payloads."""

import asyncio
import json
from pathlib import Path
from typing import cast

import aiohttp


class RpcError(RuntimeError):
    pass


class ConnectionLost(RpcError):
    pass


class Rpc:
    def __init__(self, send, incoming, close):
        self._send = send
        self._incoming = incoming
        self._close = close
        self._pending: dict[int, asyncio.Future[dict]] = {}
        self._sequence = 0
        self._reader = asyncio.create_task(self._read())

    async def _read(self):
        try:
            async for raw in self._incoming:
                item = json.loads(raw)
                if not isinstance(item, dict):
                    break
                # Do not answer approval, tool, or authentication requests broadcast
                # to the observer connection. Their owning client handles them.
                if "method" not in item:
                    response_id = item.get("id")
                    future = (
                        self._pending.get(response_id) if isinstance(response_id, int) else None
                    )
                    if future is not None and not future.done():
                        future.set_result(item)
        except (ValueError, OSError, aiohttp.ClientError):
            pass
        finally:
            for future in self._pending.values():
                if not future.done():
                    future.set_exception(ConnectionLost("app-server connection closed"))

    async def initialize(self):
        initialized = await self.call(
            "initialize",
            {
                "clientInfo": {"name": "codex_account_router", "version": "0.1.0"},
                "capabilities": {"experimentalApi": True},
            },
        )
        if initialized.get("userAgent", "").split(" ", 1)[0].rsplit("/", 1)[-1] != "0.157.1":
            raise RpcError(
                "stock app-server version needs account-router qualification (expected 0.157.1)"
            )
        await self._send({"method": "initialized"})
        return self

    async def call(self, method, params):
        if self._reader.done():
            raise ConnectionLost("app-server connection closed")
        self._sequence += 1
        request_id = self._sequence
        future = asyncio.get_running_loop().create_future()
        self._pending[request_id] = future
        try:
            async with asyncio.timeout(30):
                try:
                    await self._send({"id": request_id, "method": method, "params": params})
                except (OSError, aiohttp.ClientError):
                    raise ConnectionLost("app-server connection closed") from None
                response = await future
            if "error" in response:
                raise RpcError(f"{method} rejected ({response['error'].get('code')})")
            return response["result"]
        finally:
            self._pending.pop(request_id, None)

    async def close(self):
        await self._close()
        self._reader.cancel()
        await asyncio.gather(self._reader, return_exceptions=True)

    @classmethod
    async def local(cls, socket_path: Path):
        session = aiohttp.ClientSession(connector=aiohttp.UnixConnector(path=str(socket_path)))
        try:
            websocket = await session.ws_connect("http://localhost/", max_msg_size=32 * 1024 * 1024)

            async def incoming():
                async for message in websocket:
                    if message.type == aiohttp.WSMsgType.TEXT:
                        yield message.data

            rpc = cls(websocket.send_json, incoming(), session.close)
            try:
                return await rpc.initialize()
            except BaseException:
                await rpc.close()
                raise
        except BaseException:
            await session.close()
            raise

    @classmethod
    async def worker(cls, argv, *, home: Path, env: dict, pass_fds=()):
        process = await asyncio.create_subprocess_exec(
            *argv,
            cwd=home,
            env=env,
            pass_fds=pass_fds,
            stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.DEVNULL,
            limit=4 * 1024 * 1024,
        )
        input_stream = cast(asyncio.StreamWriter, process.stdin)
        output_stream = cast(asyncio.StreamReader, process.stdout)

        async def send(item):
            input_stream.write((json.dumps(item) + "\n").encode())
            await input_stream.drain()

        async def close():
            input_stream.close()
            try:
                await asyncio.wait_for(process.wait(), 8)
            except TimeoutError:
                process.terminate()
                await asyncio.wait_for(process.wait(), 8)

        rpc = cls(send, output_stream, close)
        try:
            return await rpc.initialize()
        except BaseException:
            await rpc.close()
            raise
