#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = ["aiohttp>=3.9"]
# ///
"""Account proxy: rate-limit-aware ChatGPT account switching outside the engine.

Spike for cbusillo/codex-lab#951. Stock Codex points `openai_base_url` at this proxy; the
proxy substitutes the credentials of the current account and, when that account reports
`usage_limit_reached`, replays the same request on the next one.

The account store is read, never written, and tokens are never refreshed: a refresh rotates
the refresh token and would sign Codex Lab out of that account. Tokens are never logged.
"""

import argparse
import json
import sys
import time
from dataclasses import dataclass
from pathlib import Path

from aiohttp import ClientSession, ClientTimeout, web

UPSTREAM = "https://chatgpt.com"
ROUTE_PREFIX = "/backend-api/codex"
MAX_REQUEST_BYTES = 64 * 1024 * 1024
DEFAULT_BLOCK_SECONDS = 15 * 60
# Hop-by-hop headers, plus the two this proxy owns and the length aiohttp recomputes.
DROPPED_REQUEST_HEADERS = {
    "host", "connection", "keep-alive", "transfer-encoding", "upgrade", "te", "trailer",
    "proxy-authorization", "proxy-connection", "content-length", "authorization",
    "chatgpt-account-id",
}
DROPPED_RESPONSE_HEADERS = {"connection", "keep-alive", "transfer-encoding", "content-length"}


@dataclass
class Account:
    label: str
    access_token: str
    account_id: str
    blocked_until: float = 0.0


def load_accounts(store: Path, start_with: str | None) -> list[Account]:
    data = json.loads(store.read_text())
    accounts = [
        Account(entry.get("label") or entry["id"][:8], entry["tokens"]["access_token"], entry["tokens"]["account_id"])
        for entry in data["accounts"]
        if entry.get("mode") == "chatgpt" and entry.get("tokens", {}).get("access_token")
    ]
    if not accounts:
        raise SystemExit(f"no ChatGPT accounts in {store}")
    active = data.get("active_account_id")
    ids = [entry["id"] for entry in data["accounts"] if entry.get("mode") == "chatgpt"]
    if active in ids:
        first = ids.index(active)
        accounts = accounts[first:] + accounts[:first]
    if start_with is not None:
        labels = [account.label for account in accounts]
        if start_with not in labels:
            raise SystemExit("--start-with does not match any account label")
        first = labels.index(start_with)
        accounts = accounts[first:] + accounts[:first]
    return accounts


def log(message: str) -> None:
    print(f"{time.strftime('%H:%M:%S')} {message}", file=sys.stderr, flush=True)


def is_usage_limit(status: int, body: bytes) -> bool:
    if status != 429:
        return False
    try:
        return json.loads(body).get("error", {}).get("type") == "usage_limit_reached"
    except (ValueError, AttributeError):
        return False


def block_seconds(headers) -> float:
    # The backend reports the window that tripped; without it, fall back to a short block.
    for name in ("x-codex-primary-reset-after-seconds", "retry-after"):
        value = headers.get(name)
        if value and value.replace(".", "", 1).isdigit():
            return float(value)
    return DEFAULT_BLOCK_SECONDS


async def forward(request: web.Request) -> web.StreamResponse:
    app = request.app
    if request.headers.get("upgrade", "").lower() == "websocket":
        # Out of scope for the spike. Codex falls back to HTTPS when the upgrade is refused.
        return web.Response(status=426, text="account-proxy: WebSocket transport is not proxied")
    body = await request.read()
    headers = {k: v for k, v in request.headers.items() if k.lower() not in DROPPED_REQUEST_HEADERS}
    url = f"{UPSTREAM}{request.rel_url}"

    limited: tuple[int, dict, bytes] | None = None
    for account in app["accounts"]:
        if account.blocked_until > time.time():
            continue
        if request.method == "POST" and account.label in app["simulate_limit"]:
            app["simulate_limit"].discard(account.label)
            account.blocked_until = time.time() + DEFAULT_BLOCK_SECONDS
            log(f"{request.method} {request.path}: SIMULATED usage limit on [{account.label}]; trying next account")
            continue
        upstream = await app["session"].request(
            request.method,
            url,
            data=body,
            headers={**headers, "Authorization": f"Bearer {account.access_token}", "ChatGPT-Account-Id": account.account_id},
            allow_redirects=False,
        )
        if upstream.status == 429:
            payload = await upstream.read()
            if is_usage_limit(upstream.status, payload):
                account.blocked_until = time.time() + block_seconds(upstream.headers)
                limited = (upstream.status, dict(upstream.headers), payload)
                log(f"{request.method} {request.path}: usage limit on [{account.label}]; trying next account")
                continue
            return web.Response(status=429, body=payload, headers=response_headers(upstream.headers))
        log(f"{request.method} {request.path}: {upstream.status} via [{account.label}]")
        response = web.StreamResponse(status=upstream.status, headers=response_headers(upstream.headers))
        await response.prepare(request)
        try:
            async for chunk in upstream.content.iter_any():
                await response.write(chunk)
            await response.write_eof()
        except ConnectionResetError:
            # The client hung up once it had what it needed.
            upstream.close()
        return response

    log(f"{request.method} {request.path}: every account is limited")
    if limited:
        status, upstream_headers, payload = limited
        return web.Response(status=status, body=payload, headers=response_headers(upstream_headers))
    return web.json_response(
        {"error": {"type": "usage_limit_reached", "message": "account-proxy: every account is limited"}},
        status=429,
    )


def response_headers(headers) -> dict:
    return {k: v for k, v in headers.items() if k.lower() not in DROPPED_RESPONSE_HEADERS}


async def start_session(app: web.Application):
    # Bodies pass through untouched, including compressed ones in either direction.
    app["session"] = ClientSession(auto_decompress=False, timeout=ClientTimeout(total=None, sock_connect=30))
    yield
    await app["session"].close()


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--store", type=Path, default=Path.home() / ".codex-lab" / "auth_accounts.json")
    parser.add_argument("--port", type=int, default=8787)
    parser.add_argument("--start-with", metavar="LABEL", help="try this account first instead of the store's active one")
    parser.add_argument(
        "--simulate-limit",
        action="append",
        default=[],
        metavar="LABEL",
        help="treat this account's next request as usage-limited without contacting the backend (spike demo)",
    )
    args = parser.parse_args()

    # Bodies pass through untouched; Codex compresses requests with zstd.
    app = web.Application(client_max_size=MAX_REQUEST_BYTES, handler_args={"auto_decompress": False})
    app["accounts"] = load_accounts(args.store, args.start_with)
    app["simulate_limit"] = set(args.simulate_limit)
    app.cleanup_ctx.append(start_session)
    app.router.add_route("*", ROUTE_PREFIX + "/{tail:.*}", forward)
    log(f"accounts in order: {[account.label for account in app['accounts']]}")
    log(f'set openai_base_url = "http://127.0.0.1:{args.port}{ROUTE_PREFIX}"')
    web.run_app(app, host="127.0.0.1", port=args.port, print=None)


if __name__ == "__main__":
    main()
