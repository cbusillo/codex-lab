"""Loopback HTTP/SSE transport; execution failures never refresh control auth."""

import asyncio
import hmac
import json

import aiohttp
from aiohttp import web

from .accounts import AccountError
from .rpc import RpcError

PREFIX = "/backend-api/codex"
DROP_HEADERS = {
    "authorization",
    "proxy-authorization",
    "www-authenticate",
    "proxy-authenticate",
    "chatgpt-account-id",
    "x-openai-account-routing-override",
    "cookie",
    "set-cookie",
    "host",
    "connection",
    "keep-alive",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
}


def headers_without_identity(headers):
    dropped = DROP_HEADERS | {
        part.strip().lower() for part in headers.get("Connection", "").split(",")
    }
    return {key: value for key, value in headers.items() if key.lower() not in dropped}


def failure(status, message):
    return web.json_response(
        {"error": {"message": message, "type": "account_router_error"}}, status=status
    )


class Proxy:
    def __init__(self, control, selection, workers, session):
        self.control = control
        self.selection = selection
        self.workers = workers
        self.session = session
        self.denied = {}

    async def authenticate(self, request):
        if "Origin" in request.headers:
            raise AccountError("browser requests are not accepted")
        supplied = request.headers.get("Authorization", "").encode()
        for _ in range(2):
            auth = await self.control.call(
                "getAuthStatus", {"includeToken": True, "refreshToken": False}
            )
            token = auth.get("authToken")
            if auth.get("authMethod") != "chatgpt" or not token:
                break
            if hmac.compare_digest(supplied, ("Bearer " + token).encode()):
                return
        raise AccountError("control caller authentication failed")

    async def handle(self, request):
        try:
            await self.authenticate(request)
            if request.headers.get("Upgrade", "").lower() == "websocket":
                return failure(426, "use HTTP/SSE for account-router model requests")
            if request.method == "GET" and request.path == PREFIX + "/models":
                # Catalog discovery has no thread identity. Use the first explicit
                # serve account; actual turns always use their own pinned selection.
                return await self.forward(request, next(iter(self.workers)))
            if request.method != "POST" or request.path not in (
                PREFIX + "/responses",
                PREFIX + "/responses/compact",
            ):
                return failure(404, "model endpoint is outside the qualified router surface")
            metadata = json.loads(request.headers.get("x-codex-turn-metadata", "{}"))
            if not isinstance(metadata, dict):
                raise AccountError("turn metadata must be an object")
            thread_id, turn_id = metadata.get("thread_id"), metadata.get("turn_id")
            if (
                not isinstance(thread_id, str)
                or not thread_id
                or not isinstance(turn_id, str)
                or not turn_id
            ):
                raise AccountError("model request lacks thread and turn identity")
            if request.headers.get("thread-id") != thread_id:
                raise AccountError("model request thread identities disagree")
            async with self.selection.route(thread_id, turn_id) as label:
                response = await self.forward(request, label)
                await self.selection.receipt(thread_id, label, response.status)
                return response
        except (AccountError, ValueError, TypeError, KeyError) as error:
            message = (
                str(error)
                if isinstance(error, AccountError)
                else "invalid model request or protocol metadata"
            )
            return failure(403, message)
        except (RpcError, TimeoutError):
            return failure(503, "owning stock server or credential worker is unavailable")

    async def forward(self, request, label):
        worker = self.workers[label]
        credential = await worker.credentials()
        if self.denied.get(label) == credential.token:
            return failure(403, f"execution account {label} needs a new login")
        body = await request.read()
        headers = headers_without_identity(request.headers)
        # Request compression is kept intact; AppRunner must disable automatic
        # decompression, just as the outgoing ClientSession does for responses.
        response = None
        try:
            for attempt in range(2):
                headers["Authorization"] = "Bearer " + credential.token
                headers["ChatGPT-Account-Id"] = credential.account_id
                headers.pop("x-openai-account-routing-override", None)
                if credential.routing != "NO_CONSTRAINT":
                    headers["x-openai-account-routing-override"] = credential.routing
                upstream = await self.session.request(
                    request.method,
                    credential.origin + request.path,
                    params=request.query,
                    data=body,
                    headers=headers,
                    allow_redirects=False,
                )
                async with upstream:
                    if upstream.status == 401:
                        if attempt == 0:
                            credential = await worker.credentials(
                                refresh=True, rejected_token=credential.token
                            )
                            continue
                        self.denied[label] = credential.token
                        return failure(403, f"execution account {label} needs a new login")
                    if 300 <= upstream.status < 400:
                        return failure(502, "execution backend redirect refused")
                    response = web.StreamResponse(
                        status=upstream.status, headers=headers_without_identity(upstream.headers)
                    )
                    await response.prepare(request)
                    async for chunk in upstream.content.iter_chunked(65536):
                        await response.write(chunk)
                    await response.write_eof()
                    return response
        except (aiohttp.ClientError, OSError, asyncio.TimeoutError):
            if response is not None and response.prepared:
                # A partial SSE stream cannot be retried or replaced with JSON.
                # Close the client connection so stock observes an incomplete turn.
                if request.transport is not None:
                    request.transport.abort()
                return response
            return failure(502, f"execution account {label} transport failed")
        raise AssertionError("bounded execution attempt did not return")


def application(proxy):
    app = web.Application(client_max_size=64 * 1024 * 1024)
    app.router.add_route("*", "/{path:.*}", proxy.handle)
    return app
