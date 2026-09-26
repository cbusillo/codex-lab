"""Stock provider credential pipe; stdout is an access token, never a log."""

import argparse
import asyncio
import sys
from pathlib import Path

import aiohttp

from .accounts import AccountError
from .rpc import Rpc, RpcError


async def control_token(socket_path):
    rpc = await Rpc.local(socket_path)
    try:
        auth = await rpc.call("getAuthStatus", {"includeToken": True, "refreshToken": False})
        token = auth.get("authToken")
        if auth.get("authMethod") != "chatgpt" or not isinstance(token, str) or not token:
            raise AccountError("control authentication is unavailable")
        return token
    finally:
        await rpc.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("socket", type=Path)
    args = parser.parse_args()
    if not args.socket.is_absolute() or sys.stdout.isatty():
        parser.exit(1, "provider auth requires an absolute local socket and a credential pipe\n")
    try:
        token = asyncio.run(control_token(args.socket))
    except (AccountError, RpcError, OSError, aiohttp.ClientError, TimeoutError, ValueError):
        parser.exit(1, "control credential lookup failed\n")
    print(token)


if __name__ == "__main__":
    main()
