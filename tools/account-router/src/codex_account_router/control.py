"""Reconnect read-only observation without replaying task/config mutations."""

import asyncio
from collections.abc import Awaitable, Callable
from pathlib import Path

import aiohttp

from .accounts import AccountError
from .rpc import ConnectionLost, Rpc, RpcError


class Control:
    def __init__(self, socket_path: Path, connect: Callable[[Path], Awaitable[Rpc]] = Rpc.local):
        self.socket_path = socket_path
        self.connect = connect
        self.rpc = None
        self.owner_id = None
        self.lock = asyncio.Lock()

    async def connection(self):
        async with self.lock:
            if self.rpc is None:
                try:
                    rpc = await self.connect(self.socket_path)
                except (OSError, aiohttp.ClientError):
                    raise ConnectionLost("control server is unavailable") from None
                try:
                    account = await rpc.call("account/read", {"refreshToken": False})
                    identity = (account.get("workspaceRouting") or {}).get("chatgptAccountId")
                    if not isinstance(identity, str) or not identity:
                        raise AccountError("control server needs a stock ChatGPT login")
                    if self.owner_id is not None and identity != self.owner_id:
                        raise AccountError(
                            "control identity changed; restart requires owner review"
                        )
                    self.owner_id = identity
                    self.rpc = rpc
                except BaseException:
                    await rpc.close()
                    raise
            return self.rpc

    async def call(self, method, params):
        if method not in ("getAuthStatus", "account/read", "thread/read") or params.get(
            "refreshToken"
        ):
            raise RpcError("control observer permits read-only calls without forced refresh")
        for attempt in range(2):
            rpc = await self.connection()
            try:
                return await rpc.call(method, params)
            except ConnectionLost:
                async with self.lock:
                    if self.rpc is rpc:
                        self.rpc = None
                        await rpc.close()
                if attempt:
                    raise
        raise AssertionError("bounded control read did not return")

    async def close(self):
        async with self.lock:
            if self.rpc is not None:
                await self.rpc.close()
                self.rpc = None
